// The unit pricing console: a price from the administrator's valuation
// through its checks, a second person's approval and publication to every
// partition, and what happens when a price is wrong or a fund is suspended.
//
// The driver that reads the administrator's files and the core's units on
// issue is not part of this stack, so each test reports those two facts to
// the module as the driver would. Everything after that is the real thing:
// the checks, the approval rules, and the delivery of an approved price to
// the core's partitions, which the test waits for before it reads a price
// "in force".
//
// The three shipped funds are shared, and a fund's prices follow one another
// with no gaps, so every test here holds its fund (`lock`) and works on the
// fund's next pricing date in order, as the module reports it.
"use strict";
const { test, Journey } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Unit pricing";
const UNITS = 1_000_000_000_000; // a million units, in micro-units
const COMMANDS = "/staff/v1/unit-pricing/commands";

const weekend = (d) => [0, 6].includes(new Date(d + "T00:00:00Z").getUTCDay());
/// The weekday after a date.
function after(d) { do { d = Journey.before(d, -1); } while (weekend(d)); return d; }
/// The fund's next pricing date in order (prices follow one another with no gaps, skipping suspended days),
/// with the net asset value and date of its latest price in force. Asked of the module, so a date an earlier
/// journey left unpriced is simply taken up by the next one.
async function next(t, fund) {
  const rows = await t.does(cast.st9, "GET", `/staff/v1/unit-pricing/prices?fund=${fund}&from=2027-01-01&to=2027-12-24`);
  const last = rows.sort((x, y) => x.pricing_date.localeCompare(y.pricing_date) || x.version - y.version).pop();
  const fundRow = (await t.does(cast.st9, "GET", "/staff/v1/unit-pricing/funds")).find((f) => f.fund === fund);
  const suspended = (d) => (fundRow.suspensions || []).some((x) => x.from <= d && (!x.to || d < x.to));
  let date = last ? last.pricing_date : "2027-01-29";
  do { date = after(date); } while (suspended(date));
  return { date, nav: last ? Math.round(Number(last.mid) * 1_000_000 * 100) / 100 : 1_000_000, inforce: last ? last.pricing_date : null };
}

/// What the driver reports: the administrator's file and the core's units on issue.
async function intake(t, command) {
  const r = await t.sys.tell("unit-pricing", { key: t.ref("intake"), caller: "unit-pricing-intake", actor: "unit-pricing-intake", roles: [], at: new Date().toISOString(), command });
  if (!r.result || r.result.Err || (r.result.Ok && r.result.Ok.refused)) throw new Error(`unit pricing refused ${Object.keys(command)[0]}: ${JSON.stringify(r).slice(0, 300)}`);
  return r.result.Ok;
}
/// The core's units on issue on every partition, once the orders of the price in force on `through` have executed.
async function core(t, fund, through) {
  for (let p = 0; p < t.stack.partitions; p++) await intake(t, { record_core_units: { fund, partition: p, executed_through: through, units_micro: p === 0 ? UNITS : 0 } });
}
/// The wrong price's date is prepared again, from the units as they stood before it.
async function corrected(t, wrong) { await core(t, wrong.fund, wrong.before); return prepared(t, wrong); }
/// A valuation arrives for the fund's next date. `move` is the change in net asset value since the last one.
async function valued(t, fund, { move = 0.0004, benchmark = move, units = UNITS } = {}) {
  const { date, nav, inforce } = await next(t, fund);
  const now = Math.round(nav * (1 + move) * 100) / 100;
  await core(t, fund, inforce);
  await intake(t, { receive_valuations: { administrator: "fa1", file_id: t.ref("file").toLowerCase(), valuations: [{ fund, pricing_date: date, nav: now.toFixed(2), units_on_issue_micro: units, applications: "0", redemptions: "0", benchmark_return: String(benchmark) }] } });
  return { fund, date, nav: now, before: inforce, mid: (now / (units / 1_000_000)).toFixed(4) };
}
const sends = (t, who, command) => t.does(who, "POST", COMMANDS, command, Object.keys(command)[0]);
async function prepared(t, v) { return (await sends(t, cast.st8, { prepare_price: { fund: v.fund, pricing_date: v.date } })).output.prepared.price; }
/// Wait until the core's partitions have taken an approved price and it is in force.
async function inForce(t, v) {
  for (let n = 0; n < 75; n++) {
    const rows = await t.does(cast.st9, "GET", `/staff/v1/unit-pricing/prices?fund=${v.fund}&from=${v.date}&to=${v.date}`);
    if (rows.length) return rows[0];
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`the price of ${v.fund} for ${v.date} never came into force`);
}
/// A price in force for the fund's next date: valued, prepared, approved by a second person, delivered.
async function priced(t, fund, options) {
  const v = await valued(t, fund, options);
  const price = await prepared(t, v);
  await sends(t, cast.st9, { approve_price: { price, accepted: {} } });
  await inForce(t, v);
  return { ...v, price };
}
async function inRow(s, table, text, button) {
  await s.sees(text, table);
  await s.stale();
  await s.row(table, text).getByRole("button", { name: button, exact: true }).click();
}
const buttons = (s, table, text) => s.row(table, text).getByRole("button").allInnerTexts();
/// The page has finished loading its lists (buttons are live until then).
const loaded = (s) => s.page.waitForFunction(() => document.querySelector("#pfund").options.length > 0);
const cash = { lock: "pricing:cash" }, balanced = { lock: "pricing:balanced" }, growth = { lock: "pricing:growth" };

// --- Who sees what -----------------------------------------------------------------------------------------------

test({ id: "E2E-210", area: A, persona: "ST8", story: "US-ST08-1", title: "The pricing maker arrives as a preparer with the Prepare button live", matrix: ["INV-007"],
  purpose: "Sign in to the unit pricing console as the maker.",
  why: "The console must tell each person which half of the maker-checker control is theirs." }, async (t) => {
  const s = await t.open("/pricing", cast.st8);
  await s.sees("preparer", "#roles");
  await loaded(s);
  await s.tab("Prices");
  if (await s.page.locator("#prepare button").first().isDisabled()) throw new Error("the maker cannot press Prepare");
  await s.tab("Funds");
  if (!(await s.page.locator("#suspend button").isDisabled())) throw new Error("the maker can press the button that suspends a fund");
});

test({ id: "E2E-211", area: A, persona: "ST9", story: "US-ST09-1", title: "The checker cannot prepare a price", matrix: ["INV-008"],
  purpose: "Sign in as the checker and look at the Prepare button.",
  why: "A checker who can also prepare could approve their own work under another name; the roles are kept apart." }, async (t) => {
  const s = await t.open("/pricing", cast.st9);
  await s.sees("approver", "#roles");
  await loaded(s);
  await s.tab("Prices");
  if (!(await s.page.locator("#prepare button").first().isDisabled())) throw new Error("the checker can press Prepare");
});

test({ id: "E2E-212", area: A, persona: "ST10", story: "US-ST09-1", title: "Staff with no unit pricing role are refused the console", matrix: ["INV-008"],
  purpose: "Open the unit pricing console as the finance operator, who holds no unit pricing role.",
  why: "Unpublished prices are market-sensitive inside the insurer; only the pricing team sees them." }, async (t) => {
  const s = await t.open("/pricing", cast.st10, 403);
  await s.refused();
  await s.never("Prepare a price");
});

// --- A price from valuation to in force (the cash fund) ----------------------------------------------------------

test({ id: "E2E-213", area: A, persona: "ST8", story: "US-ST08-1", title: "A price cannot be prepared before the administrator's valuation arrives", matrix: ["INV-002"], ...cash,
  purpose: "Press Prepare for a fund and date no valuation has been received for.",
  why: "A price is struck from the administrator's figures and nothing else; with no file there is no price." }, async (t) => {
  const date = "2027-12-20"; // a date no journey reaches
  const s = await t.open("/pricing", cast.st8);
  await s.tab("Prices");
  await s.pick("#pfund", "cash");
  await s.fill("#pdate", date);
  await s.press("Prepare", "#prepare");
  await s.stopped("#prepare-msg", `no valuation of cash:${date} has been received`, 409);
});

test({ id: "E2E-214", area: A, persona: "ST8", story: "US-FA1-1", title: "The maker can read the valuation the administrator sent before preparing", matrix: ["INV-006"], ...cash,
  purpose: "Press Show the valuation received for a fund and date with a file.",
  why: "The maker's first check is the administrator's own figures: net asset value and units on issue." }, async (t) => {
  const v = await valued(t, "cash");
  const s = await t.open("/pricing", cast.st8);
  await s.tab("Prices");
  await s.pick("#pfund", "cash");
  await s.fill("#pdate", v.date);
  await s.press("Show the valuation received", "#prepare");
  await s.sees(v.nav.toFixed(2), "#valuation");
  await s.sees("fa1", "#valuation");
});

test({ id: "E2E-215", area: A, persona: "ST8", story: "US-ST08-1", title: "The maker prepares a price and it waits for someone else", matrix: ["INV-001"], ...cash,
  purpose: "Prepare a price from a valuation that passes every check, and read Work waiting.",
  why: "A prepared price is not yet a price; it must sit in the queue, under the maker's name, for a second person." }, async (t) => {
  const v = await valued(t, "cash");
  const s = await t.open("/pricing", cast.st8);
  await s.tab("Prices");
  await s.pick("#pfund", "cash");
  await s.fill("#pdate", v.date);
  await s.press("Prepare", "#prepare");
  const said = await s.done("#prepare-msg", ": prepared. Someone else approves it.");
  const price = said.match(/Prepared (\S+):/)[1];
  if (!price.startsWith(`cash:${v.date}.`)) throw new Error(`the price prepared is ${price}`);
  await s.tab("Work waiting");
  await s.sees(price, "#work");
  const cells = await s.row("#work", price).locator("td").allInnerTexts();
  if (cells[3] !== v.mid || cells[6] !== "prepared" || cells[8] !== "st8") throw new Error(`the waiting price reads ${JSON.stringify(cells)}`);
  if (!(Number(cells[4]) > Number(cells[3]) && Number(cells[3]) > Number(cells[5]))) throw new Error(`buy, mid and sell are not spread around the mid: ${cells.slice(3, 6)}`);
});

test({ id: "E2E-216", area: A, persona: "ST8", story: "US-ST09-1", title: "The maker is offered no way to approve or reject their own price", matrix: ["INV-008"], ...cash,
  purpose: "As the maker, read the buttons beside their own prepared price.",
  why: "The story is that a checker cannot approve their own price; it starts with the maker having no approval to give." }, async (t) => {
  const v = await valued(t, "cash");
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st8);
  await s.sees(price, "#work");
  const offered = await buttons(s, "#work", price);
  if (offered.includes("Approve") || offered.includes("Reject")) throw new Error(`the maker is offered ${JSON.stringify(offered)}`);
});

test({ id: "E2E-217", area: A, persona: "ST8", story: "US-ST09-1", title: "A maker who sends an approval by hand is refused", matrix: ["INV-008"], ...cash,
  purpose: "As the maker, send the approval of their own prepared price straight to the console's route.",
  why: "Only an approved price activates; the module itself must refuse the preparer, not just the page." }, async (t) => {
  const v = await valued(t, "cash");
  const price = await prepared(t, v);
  const r = await t.sys.portal("POST", COMMANDS, cast.st8, { approve_price: { price, accepted: {} } });
  if (r.status !== 403 || !String(r.data.detail).includes("unit_pricing_approver")) throw new Error(`the approval answered ${r.status} ${JSON.stringify(r.data)}`);
  const s = await t.open("/pricing", cast.st9);
  const cells = await (await s.sees(price, "#work"), s.row("#work", price).locator("td").allInnerTexts());
  if (cells[6] !== "prepared") throw new Error(`after the refused approval the price reads ${cells[6]}`);
});

test({ id: "E2E-218", area: A, persona: "ST9", story: "US-ST09-1", title: "The checker approves a price and it comes into force on every partition", matrix: ["INV-001"], ...cash,
  purpose: "As the checker, approve a prepared price, then look it up and open its details.",
  why: "Approval is what publishes a price to the core; the checker must be able to see that every partition took it." }, async (t) => {
  const v = await valued(t, "cash");
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9);
  await inRow(s, "#work", price, "Approve");
  await s.done("#work-msg", "Approved. It is on its way to every partition.");
  await inForce(t, v);
  await s.tab("Prices");
  await s.pick("#lfund", "cash");
  await s.fill("#lfrom", v.date);
  await s.fill("#lto", v.date);
  await s.press("Show prices in force", "#lookup");
  await inRow(s, "#prices", price, "Details");
  await s.sees(`${price}: active`, "#detail-title");
  const partitions = await s.rows("#detail-deliveries");
  if (partitions.length !== t.stack.partitions || partitions.some((p) => p[1] !== "applied")) throw new Error(`the partitions read ${JSON.stringify(partitions)}`);
  const checks = await s.rows("#detail-checks");
  if (checks.length < 5 || checks.some((c) => c[1] !== "passed")) throw new Error(`the checks read ${JSON.stringify(checks)}`);
});

test({ id: "E2E-219", area: A, persona: "ST9", story: "US-ST09-1", title: "The checker rejects a price with a reason and the maker prepares it again", matrix: ["INV-003"], ...cash,
  purpose: "Reject a prepared price, giving a reason; as the maker, prepare the same date again.",
  why: "A rejected price must leave the queue with its reason, and the date must stay open for a second attempt." }, async (t) => {
  const v = await valued(t, "cash");
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9);
  await inRow(s, "#work", price, "Reject");
  await s.answer("The file was restated after this was prepared.");
  await s.done("#work-msg", "Rejected.");
  await s.never(price, "#work");
  const again = await prepared(t, v);
  if (again === price || !again.startsWith(`cash:${v.date}.`)) throw new Error(`the second attempt is ${again}`);
  await sends(t, cast.st9, { approve_price: { price: again, accepted: {} } });
  await inForce(t, v);
});

test({ id: "E2E-220", area: A, persona: "ST8", story: "US-FA1-1", title: "A price is blocked when the administrator's units on issue differ from the core's", matrix: ["INV-006"], ...cash,
  purpose: "Prepare a price from a valuation whose units on issue are one micro-unit off the core's, and read it as the checker.",
  why: "A unit difference means someone holds units the fund does not; no price may be approved over it, by anyone." }, async (t) => {
  const v = await valued(t, "cash", { units: UNITS + 1 });
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9);
  await s.sees(price, "#work");
  const cells = await s.row("#work", price).locator("td").allInnerTexts();
  if (cells[6] !== "blocked" || !cells[7].includes("units on issue")) throw new Error(`the price reads ${JSON.stringify(cells.slice(6, 8))}`);
  const offered = await buttons(s, "#work", price);
  if (offered.some((b) => b.includes("pprove"))) throw new Error(`the checker is offered ${JSON.stringify(offered)} on a blocked price`);
  await inRow(s, "#work", price, "Reject");
  await s.answer("Units do not agree with the administrator.");
  await s.done("#work-msg", "Rejected.");
});

test({ id: "E2E-221", area: A, persona: "ST8", story: "US-ST08-1", title: "A file that could not be read is listed with the reason", matrix: ["INV-006"],
  purpose: "Have the intake refuse a valuation file and read the list of files that could not be read.",
  why: "A file that silently fails to load looks like a late administrator; the team must see that it arrived and why it was refused." }, async (t) => {
  const file = t.ref("badfile").toLowerCase();
  await intake(t, { refuse_valuation_file: { file_id: file, reason: "row 3: net asset value is not a number" } });
  const s = await t.open("/pricing", cast.st8);
  await s.sees(file, "#refused");
  await s.sees("row 3: net asset value is not a number", "#refused");
});

// --- Checks that hold a price (the balanced fund) ----------------------------------------------------------------

test({ id: "E2E-222", area: A, persona: "ST8", story: "US-ST08-1", title: "A 9% move against a 1% benchmark holds the price for review", matrix: ["INV-007"], ...balanced,
  purpose: "With a price in force, prepare the next date from a valuation 9% higher while the benchmark moved 1%.",
  why: "This is the story's own case: the checks must stop a bad price before anyone trades on it." }, async (t) => {
  await priced(t, "balanced");
  const v = await valued(t, "balanced", { move: 0.09, benchmark: 0.01 });
  const s = await t.open("/pricing", cast.st8);
  await s.tab("Prices");
  await s.pick("#pfund", "balanced");
  await s.fill("#pdate", v.date);
  await s.press("Prepare", "#prepare");
  const price = (await s.done("#prepare-msg", ": held.")).match(/Prepared (\S+):/)[1];
  await s.tab("Work waiting");
  const cells = await (await s.sees(price, "#work"), s.row("#work", price).locator("td").allInnerTexts());
  if (cells[6] !== "held" || !cells[7].includes("previous move") || !cells[7].includes("benchmark")) throw new Error(`the held price reads ${JSON.stringify(cells.slice(6, 8))}`);
  await sends(t, cast.st9, { reject_price: { price, reason: "end of test" } });
});

test({ id: "E2E-223", area: A, persona: "ST9", story: "US-ST08-1", title: "A held price is approved only by accepting each failed check with a reason", matrix: ["INV-007"], ...balanced,
  purpose: "As the checker, press Accept checks and approve on a held price, answer each check's question, and open the details.",
  why: "A real market move can fail a check; overriding it must be a recorded decision, check by check, not a click." }, async (t) => {
  await priced(t, "balanced");
  const v = await valued(t, "balanced", { move: 0.04, benchmark: 0.04 });
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9);
  await s.sees(price, "#work");
  const failed = (await s.row("#work", price).locator("td").allInnerTexts())[7];
  if (!failed.includes("previous move")) throw new Error(`a 4% move did not fail the previous move check: "${failed}"`);
  await inRow(s, "#work", price, "Accept checks and approve");
  for (let n = failed.split(";").length; n > 0; n--) await s.answer("Market fell 4% on the day; benchmark agrees.").then(() => s.wait(300));
  await s.done("#work-msg", "Approved. It is on its way to every partition.");
  await inForce(t, v);
  await s.tab("Prices");
  await s.pick("#lfund", "balanced");
  await s.fill("#lfrom", v.date);
  await s.fill("#lto", v.date);
  await s.press("Show prices in force", "#lookup");
  await inRow(s, "#prices", price, "Details");
  await s.sees(`${price}:`, "#detail-title");
  const move = (await s.rows("#detail-checks")).find((c) => c[0] === "previous move");
  if (!move || move[1] !== "failed" || !move[3].includes("Market fell 4%")) throw new Error(`the previous move check reads ${JSON.stringify(move)}`);
});

test({ id: "E2E-224", area: A, persona: "ST9", story: "US-ST08-1", title: "A held price cannot be approved without accepting its failed checks", matrix: ["INV-007"], ...balanced,
  purpose: "Send a plain approval, with no accepted checks, for a held price.",
  why: "If a held price could be approved like any other, the checks would be decoration." }, async (t) => {
  await priced(t, "balanced");
  const v = await valued(t, "balanced", { move: 0.09, benchmark: 0.01 });
  const price = await prepared(t, v);
  const r = await t.sys.portal("POST", COMMANDS, cast.st9, { approve_price: { price, accepted: {} } });
  if (r.ok) throw new Error("a held price was approved with no check accepted");
  if (r.status !== 409) throw new Error(`the approval answered ${r.status} ${JSON.stringify(r.data)}`);
  const s = await t.open("/pricing", cast.st9);
  const cells = await (await s.sees(price, "#work"), s.row("#work", price).locator("td").allInnerTexts());
  if (cells[6] !== "held") throw new Error(`the price now reads ${cells[6]}`);
  await sends(t, cast.st9, { reject_price: { price, reason: "end of test" } });
});

// --- Price errors (the balanced fund) ----------------------------------------------------------------------------

test({ id: "E2E-225", area: A, persona: "ST9", story: "US-P21-1", title: "The lead raises a price error and it shows as open and not yet corrected", matrix: ["INV-010"], ...balanced,
  purpose: "As the unit pricing lead, raise an error against a price in force and read the error list; then correct and close it.",
  why: "A wrong price has to become a tracked error the moment it is found, naming which prices are wrong and whether each is corrected." }, async (t) => {
  const wrong = await priced(t, "balanced");
  const s = await t.open("/pricing", cast.st9l);
  await s.tab("Price errors");
  await s.fill("#eprices", wrong.price);
  await s.fill("#ecause", "Administrator priced one bond stale");
  await s.fill("#edate", wrong.date);
  await s.press("Raise a price error", "#raise");
  await s.done("#error-msg", "Raised. Prepare and approve the correction for each wrong price.");
  const cells = await (await s.sees(wrong.price, "#errors"), s.row("#errors", wrong.price).locator("td").allInnerTexts());
  if (!cells[4].includes("not yet corrected") || cells[6] !== "open") throw new Error(`the error reads ${JSON.stringify(cells)}`);
  // Correct it so the fund is free for the next journey.
  const fix = await corrected(t, wrong);
  await sends(t, cast.st9, { approve_price: { price: fix, accepted: {} } });
});

test({ id: "E2E-226", area: A, persona: "ST9", story: "US-P21-1", title: "No new date of a fund can be approved while a price error is uncorrected", matrix: ["INV-010"], ...balanced,
  purpose: "With an error open on yesterday's price, prepare the next date and try to approve it; then correct the wrong price and try again.",
  why: "Pricing on from a wrong price compounds the error into every later trade; the fund must stop until the correction is in." }, async (t) => {
  const wrong = await priced(t, "balanced");
  await sends(t, cast.st9l, { raise_price_error: { error: t.ref("err").toLowerCase(), prices: [wrong.price], cause: "wrong accrual", detected_on: wrong.date } });
  const v = await valued(t, "balanced");
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9);
  await inRow(s, "#work", price, "Approve");
  await s.stopped("#work-msg", "", 409);
  const fix = await corrected(t, wrong);
  await sends(t, cast.st9, { approve_price: { price: fix, accepted: {} } });
  for (let n = 0; n < 50; n++) { const p = await t.does(cast.st9, "GET", `/staff/v1/unit-pricing/price?id=${encodeURIComponent(fix)}`); if (p.status === "active") break; await s.wait(200); }
  // The correction changed what the next date follows, so that date is prepared again and then approved.
  await core(t, "balanced", wrong.date);
  const second = await prepared(t, v);
  await s.page.reload();
  await inRow(s, "#work", second, "Approve");
  await s.done("#work-msg", "Approved.");
  await inForce(t, v);
});

test({ id: "E2E-227", area: A, persona: "ST9", story: "US-P21-1", title: "A corrected error shows its correction and the lead closes it with a summary", matrix: ["INV-011"], ...balanced,
  purpose: "Raise an error, correct the wrong price, then as the lead read the error and close it.",
  why: "The error record is what the regulator and the customers' remediation rest on: the correction, whether it was material, and how it ended." }, async (t) => {
  const wrong = await priced(t, "balanced");
  await sends(t, cast.st9l, { raise_price_error: { error: t.ref("err").toLowerCase(), prices: [wrong.price], cause: "late corporate action", detected_on: wrong.date } });
  const fix = await corrected(t, wrong);
  await sends(t, cast.st9, { approve_price: { price: fix, accepted: {} } });
  const s = await t.open("/pricing", cast.st9l);
  await s.tab("Price errors");
  for (let n = 0; n < 50; n++) { await s.page.reload(); await s.tab("Price errors"); await s.sees(wrong.price, "#errors"); if ((await s.text("#errors")).includes(fix)) break; await s.wait(200); }
  const cells = await s.row("#errors", wrong.price).locator("td").allInnerTexts();
  if (!cells[4].includes(fix) || !/material/.test(cells[4])) throw new Error(`the error reads ${JSON.stringify(cells)}`);
  await inRow(s, "#errors", wrong.price, "Close");
  await s.answer("Same inputs re-struck; no holder was affected.");
  await s.done("#error-msg", "Closed.");
  await s.sees("closed by st9l", "#errors");
});

test({ id: "E2E-228", area: A, persona: "ST9", story: "US-P21-1", title: "An error cannot be raised against a price that was never in force", matrix: ["INV-010"], ...balanced,
  purpose: "As the lead, raise a price error naming a price that is only prepared.",
  why: "Only a price customers could have traded on can be wrong in the sense that needs remediation." }, async (t) => {
  const v = await valued(t, "balanced");
  const price = await prepared(t, v);
  const s = await t.open("/pricing", cast.st9l);
  await s.tab("Price errors");
  await s.fill("#eprices", price);
  await s.fill("#ecause", "looked wrong");
  await s.fill("#edate", v.date);
  await s.press("Raise a price error", "#raise");
  await s.stopped("#error-msg", "only a price in force can be wrong", 409);
  await sends(t, cast.st9, { approve_price: { price, accepted: {} } });
  await inForce(t, v);
});

// --- Suspension (the growth fund) --------------------------------------------------------------------------------

test({ id: "E2E-229", area: A, persona: "ST9", story: "US-FA1-1", title: "The lead suspends a fund and no price of it can be approved from that date", matrix: ["INV-014"], ...growth,
  purpose: "As the lead, suspend pricing of a fund from a date; as the checker, try to approve a price for that date; then resume the fund.",
  why: "When a fund cannot be valued fairly, trading stops; an approval slipping through would let people in or out at a price nobody stands behind." }, async (t) => {
  await priced(t, "growth");
  const v = await valued(t, "growth");
  const price = await prepared(t, v);
  const reopens = after(v.date);
  const s = await t.open("/pricing", cast.st9l);
  await s.tab("Funds");
  await s.pick("#sfund", "growth");
  await s.fill("#sfrom", v.date);
  await s.fill("#sreason", "Underlying market closed");
  await s.press("Send", "#suspend");
  await s.done("#fund-msg", "Suspended.");
  await s.sees(`from ${v.date}`, "#funds");
  await s.sees("Underlying market closed (st9l)", "#funds");
  await s.tab("Work waiting");
  await inRow(s, "#work", price, "Approve");
  await s.stopped("#work-msg", "suspend", 409);
  await s.tab("Funds");
  await s.pick("#sfund", "growth");
  await s.pick("#sdo", "resume_fund");
  await s.fill("#sfrom", reopens);
  await s.fill("#sreason", "Market reopened");
  await s.press("Send", "#suspend");
  await s.done("#fund-msg", "Resumed.");
  await s.sees(`from ${v.date} to ${reopens}`, "#funds");
});

test({ id: "E2E-230", area: A, persona: "ST9", story: "US-FA1-1", title: "A fund cannot be resumed from before the date it was suspended", matrix: ["INV-014"], ...growth,
  purpose: "Suspend a fund from a date and try to resume it from the same date.",
  why: "A resumption that erased the suspension would rewrite which days the fund traded." }, async (t) => {
  const from = (await next(t, "growth")).date, reopens = after(from);
  await sends(t, cast.st9l, { suspend_fund: { fund: "growth", from, reason: "Custodian outage" } });
  const s = await t.open("/pricing", cast.st9l);
  await s.tab("Funds");
  await s.pick("#sfund", "growth");
  await s.pick("#sdo", "resume_fund");
  await s.fill("#sfrom", from);
  await s.fill("#sreason", "Outage over");
  await s.press("Send", "#suspend");
  await s.stopped("#fund-msg", "a fund resumes after the date it was suspended from", 409);
  await sends(t, cast.st9l, { resume_fund: { fund: "growth", from: reopens, reason: "Outage over" } });
});

test({ id: "E2E-231", area: A, persona: "ST9", story: "US-ST09-1", title: "A checker who is not the lead cannot suspend a fund", matrix: ["INV-014"],
  purpose: "As the checker, look at the suspend form and send a suspension by hand.",
  why: "Suspending a fund stops every customer trading in it; it is the lead's decision, not any checker's." }, async (t) => {
  const s = await t.open("/pricing", cast.st9);
  await loaded(s);
  await s.tab("Funds");
  if (!(await s.page.locator("#suspend button").isDisabled())) throw new Error("the checker can press the button that suspends a fund");
  const r = await t.sys.portal("POST", COMMANDS, cast.st9, { suspend_fund: { fund: "growth", from: "2028-06-01", reason: "by hand" } });
  if (r.status !== 403) throw new Error(`the suspension answered ${r.status} ${JSON.stringify(r.data)}`);
});

test({ id: "E2E-232", area: A, persona: "ST8", story: "US-P03-1", title: "Prices in force are listed for a range of dates, each with its buy and sell either side of the mid", matrix: ["INV-001", "INV-024"], ...growth,
  purpose: "With two prices in force on consecutive dates, look up the range and read the list.",
  why: "Every unit trade takes its price from this list; the spread and the four-decimal rounding are what customers are charged." }, async (t) => {
  const one = await priced(t, "growth"), two = await priced(t, "growth");
  const s = await t.open("/pricing", cast.st8);
  await s.tab("Prices");
  await s.pick("#lfund", "growth");
  await s.fill("#lfrom", one.date);
  await s.fill("#lto", two.date);
  await s.press("Show prices in force", "#lookup");
  await s.sees(two.price, "#prices");
  const rows = (await s.rows("#prices")).filter((r) => [one.price, two.price].includes(r[0]));
  if (rows.length !== 2) throw new Error(`the list reads ${JSON.stringify(rows)}`);
  for (const r of rows) {
    if (![r[3], r[4], r[5]].every((p) => /^\d+\.\d{4}$/.test(p))) throw new Error(`a price is not to four decimals: ${JSON.stringify(r)}`);
    if (!(Number(r[4]) > Number(r[3]) && Number(r[3]) > Number(r[5]))) throw new Error(`buy, mid and sell are out of order: ${JSON.stringify(r)}`);
  }
});
