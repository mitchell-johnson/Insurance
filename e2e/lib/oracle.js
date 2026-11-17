// An independent calculator for the money and dates of a policy's life.
//
// The lifecycle tests compare what the running system shows with what this
// file works out. It shares no code with the system: it reads the shipped
// product and rate files as text (the same files an actuary would read), and
// does its own arithmetic in whole cents with its own rounding. If the
// system and this file disagree, one of them is wrong, and the test says
// which figures differ.
//
// What it knows (from `products/*.star` and the design notes):
//   - a stepped cover costs rate-per-thousand x sum insured / 1000 a year, at
//     the life's age last birthday on the date the premium is struck;
//   - a per-life cover (health) costs the table's annual rate for each life;
//   - paying more often than yearly adds the product's modal loading, and the
//     year is split into 12 monthly or 26 fortnightly instalments;
//   - a multi-cover discount is a percentage off each cover's annual premium;
//   - each line is rounded once, half-even to the cent, at the end: a cover's
//     instalment is its annual premium, less the discount, with the loading,
//     split into instalments, then rounded. The discount shown is what is
//     left between that and the same sum without the discount;
//   - the policy fee is the annual fee split the same way, without loading;
//   - GST is a percentage of the cover's instalment, rounded half-even;
//   - billing periods run from the inception day of the month to the same
//     day of the next; an unpaid premium suspends cover after the grace days
//     and lapses the policy after the lapse days, both counted from its due
//     date;
//   - a fund's price is its net assets over its units on issue: the mid price
//     is that rounded half-even to the fund's decimals, the buy price is it
//     plus the buy spread rounded up, and the sell price is it less the sell
//     spread rounded down (`products/funds/*.star`, `docs/unit-pricing.md`);
//   - a group life member's cover is a multiple of salary by category, in place
//     up to the automatic acceptance limit, at an annual rate per thousand by
//     category (`products/group_life.star`, `products/rates/group_life_standard.star`);
//     on a scheme whose cover over the retention is underwritten, what is in
//     place is also no more than the group treaty's retention left on the life
//     (`treaties/group_life_surplus.star`, `docs/reinsurance.md`).
"use strict";
const fs = require("fs");
const path = require("path");

const ROOT = path.resolve(__dirname, "../..", "products");
const read = (file) => fs.readFileSync(path.join(ROOT, file), "utf8");

// --- Exact arithmetic: amounts are BigInt numerators over a power of ten. ---------------------------------------
/// "1.8296" -> { n: 18296n, scale: 4 }
function dec(text) {
  const [whole, frac = ""] = String(text).trim().split(".");
  return { n: BigInt(whole + frac), scale: frac.length };
}
/// Round n / d half-even to an integer.
function halfEven(n, d) {
  const q = n / d, r = n % d;
  if (r * 2n > d || (r * 2n === d && q % 2n === 1n)) return q + 1n;
  return q;
}

// --- The shipped files, read as text. ---------------------------------------------------------------------------
function rateTable(id) {
  const file = fs.readdirSync(path.join(ROOT, "rates")).find((f) => read(`rates/${f}`).includes(`id = "${id}"`));
  if (!file) throw new Error(`no rate table ${id}`);
  const rows = [];
  for (const m of read(`rates/${file}`).matchAll(/rate\("([\d.]+)"((?:,\s*\w+\s*=\s*[^,)]+)*)\)/g)) {
    const row = { rate: dec(m[1]) };
    for (const kv of m[2].matchAll(/(\w+)\s*=\s*("?)([^,")]+)\2/g)) row[kv[1]] = kv[2] ? kv[3] : kv[3] === "True" ? true : kv[3] === "False" ? false : Number(kv[3]);
    rows.push(row);
  }
  return rows;
}
function product(code) {
  const file = fs.readdirSync(ROOT).filter((f) => f.endsWith(".star")).find((f) => read(f).includes(`code = "${code}"`));
  if (!file) throw new Error(`no product ${code}`);
  const text = read(file);
  const covers = {};
  // Each cover( ... ) block: its code, how it is rated, and any GST.
  for (const block of text.split(/\n\s{8}cover\(/).slice(1)) {
    const code = /code = "([^"]+)"/.exec(block)[1];
    const rating = /rating = (\w+)\("([^"]+)"\)/.exec(block);
    const gst = /gst_rate = "([\d.]+)"/.exec(block);
    covers[code] = { how: rating[1], table: rating[2], gst: gst ? dec(gst[1]) : null };
  }
  const fee = /policy_fee_annual = "([\d.]+)"/.exec(text);
  const loading = {};
  const modal = /modal_loading = \{([^}]*)\}/.exec(text);
  if (modal) for (const m of modal[1].matchAll(/"(\w+)":\s*"([\d.]+)"/g)) loading[m[1]] = dec(m[2]);
  const discount = /multi_cover_discount\(min_covers = (\d+), percent = "([\d.]+)"\)/.exec(text);
  const arrears = /arrears\(([^)]*)\)/.exec(text)[1];
  const num = (name) => { const m = new RegExp(`${name} = (\\d+)`).exec(arrears); return m ? Number(m[1]) : null; };
  const cooling = /cooling_off\(days = (\d+)\)/.exec(text);
  return {
    code, covers, fee: fee ? dec(fee[1]) : dec("0"), loading,
    discount: discount ? { covers: Number(discount[1]), percent: dec(discount[2]) } : null,
    graceDays: num("grace_days"), lapseDays: num("lapse_days"), suspends: /suspend_after_grace = True/.test(arrears),
    reinstatementDays: num("reinstatement_days"), coolingOffDays: cooling ? Number(cooling[1]) : null,
  };
}

// --- Dates. -----------------------------------------------------------------------------------------------------
const day = (text) => { const [y, m, d] = text.split("-").map(Number); return { y, m, d }; };
const iso = ({ y, m, d }) => `${y}-${String(m).padStart(2, "0")}-${String(d).padStart(2, "0")}`;
const daysIn = (y, m) => new Date(Date.UTC(y, m, 0)).getUTCDate();
/// Age last birthday on a date.
function age(born, on) {
  const b = day(born), o = day(on);
  return o.y - b.y - (o.m < b.m || (o.m === b.m && o.d < b.d) ? 1 : 0);
}
function addDays(date, n) { return new Date(Date.parse(`${date}T00:00:00Z`) + n * 86400000).toISOString().slice(0, 10); }
/// The same day of the month `n` months on, counted from the inception date (a 31st falls back to the month's last day).
function addMonths(date, n) {
  const a = day(date), total = a.m - 1 + n, y = a.y + Math.floor(total / 12), m = (((total % 12) + 12) % 12) + 1;
  return iso({ y, m, d: Math.min(a.d, daysIn(y, m)) });
}
const PERIODS = { monthly: 12n, fortnightly: 26n, yearly: 1n };
/// The billing period `n` (from 1) of a policy.
function period(inception, frequency, n) {
  if (frequency === "fortnightly") return { start: addDays(inception, 14 * (n - 1)), end: addDays(inception, 14 * n) };
  const step = frequency === "yearly" ? 12 : 1;
  return { start: addMonths(inception, step * (n - 1)), end: addMonths(inception, step * n) };
}

// --- Premium. ---------------------------------------------------------------------------------------------------
function rateFor(table, factors, on) {
  const a = age(factors.date_of_birth, on);
  const row = rateTable(table).find((r) => a >= r.age_from && a <= r.age_to && (r.sex === undefined || r.sex === factors.sex) && (r.smoker === undefined || r.smoker === Boolean(factors.smoker)));
  if (!row) throw new Error(`no ${table} rate for age ${a}, ${factors.sex}, smoker ${factors.smoker}`);
  return row.rate;
}
/// What one instalment of an application costs, struck on `on` (its inception unless given). All amounts in cents.
function premium(application, on = application.inception) {
  const p = product(application.product_code);
  const periods = PERIODS[application.frequency];
  const load = p.loading[application.frequency] || dec("0");
  const gross = {}, gst = {};
  for (const c of application.covers) {
    const spec = p.covers[c.code];
    const life = application.lives.find((l) => l.id === c.life);
    const rate = rateFor(spec.table, life.factors, on);
    // The annual premium in cents as a fraction: stepped is rate x sum insured (cents) / 1000; per life is the rate itself in cents.
    let n = spec.how === "stepped" ? rate.n * BigInt(c.sum_insured_minor) : rate.n * 100n;
    let d = 10n ** BigInt(rate.scale) * (spec.how === "stepped" ? 1000n : 1n);
    n *= 10n ** BigInt(load.scale) + load.n; d *= 10n ** BigInt(load.scale); // the modal loading
    d *= periods;
    // Per-life covers of the same code are added before rounding (one line on the notice); stepped covers round each.
    gross[c.code] = gross[c.code] || { n: 0n, d: 1n };
    gross[c.code] = { n: gross[c.code].n * d + n * gross[c.code].d, d: gross[c.code].d * d };
  }
  const byCoverGross = {}, byCover = {};
  const distinct = Object.keys(gross).length;
  const pct = p.discount && distinct >= p.discount.covers ? p.discount.percent : null;
  for (const [code, f] of Object.entries(gross)) {
    byCoverGross[code] = halfEven(f.n, f.d);
    // Less the discount, then one rounding: annual x (100 - percent) / 100, split and loaded as above.
    byCover[code] = pct ? halfEven(f.n * (100n * 10n ** BigInt(pct.scale) - pct.n), f.d * 100n * 10n ** BigInt(pct.scale)) : byCoverGross[code];
    const spec = p.covers[code];
    if (spec.gst) gst[code] = halfEven(byCover[code] * spec.gst.n, 10n ** BigInt(spec.gst.scale));
  }
  const discountTotal = Object.keys(gross).reduce((a, code) => a + byCoverGross[code] - byCover[code], 0n);
  const fee = halfEven(p.fee.n * 100n, 10n ** BigInt(p.fee.scale) * periods);
  const sum = (o) => Object.values(o).reduce((a, b) => a + b, 0n);
  const num = (o) => Object.fromEntries(Object.entries(o).map(([k, v]) => [k, Number(v)]));
  return { gross: num(byCoverGross), discount: Number(discountTotal), net: num(byCover), gst: num(gst), fee: Number(fee), total: Number(sum(byCover) + sum(gst) + fee) };
}

// --- Arrears. ---------------------------------------------------------------------------------------------------
/// For a premium due on `due` and never paid: the day cover is suspended and the day the policy lapses.
function arrears(productCode, due) {
  const p = product(productCode);
  return { graceEnds: addDays(due, p.graceDays), suspends: p.suspends, lapses: addDays(due, p.lapseDays), reinstateBy: p.reinstatementDays ? addDays(addDays(due, p.lapseDays), p.reinstatementDays) : null };
}

// --- Units. -----------------------------------------------------------------------------------------------------
/// A fund's prices for a day, from its net asset value (dollars and cents) and its units on issue (micro-units). The
/// unrounded price is net assets over units. The mid price is that rounded half-even to the fund's decimals; the buy
/// price is it plus the buy spread, rounded up; the sell price is it less the sell spread, rounded down (a customer
/// never buys below, or sells above, the exact price). Spreads (basis points) and decimals come from the fund's file.
function prices(fundCode, nav, unitsMicro) {
  const text = read(`funds/${fundCode}.star`);
  const num = (name) => BigInt(new RegExp(`${name} = (\\d+)`).exec(text)[1]);
  const decimals = Number(num("price_decimals")), unit = 10n ** BigInt(decimals), a = dec(nav);
  // price x 10^decimals = n / d, exactly.
  const n = a.n * 1_000_000n * unit, d = 10n ** BigInt(a.scale) * BigInt(unitsMicro);
  const up = (x, y) => (x % y === 0n ? x / y : x / y + 1n), down = (x, y) => x / y;
  const show = (v) => `${v / unit}.${String(v % unit).padStart(decimals, "0")}`;
  return { mid: show(halfEven(n, d)), buy: show(up(n * (10000n + num("buy_spread_bps")), d * 10000n)), sell: show(down(n * (10000n - num("sell_spread_bps")), d * 10000n)) };
}

/// A member of a group life scheme (`products/group_life.star`): cover by formula is the category's multiple of
/// salary, capped at the category's maximum; what is in place without underwriting is that, up to the cover's
/// automatic acceptance limit; and a month's premium is the category's annual rate per thousand of the cover in
/// place, over twelve, rounded half-even to the cent. Amounts are whole cents.
///
/// Where the scheme's terms send cover over the insurer's retention to underwriting (`excessPath`
/// "underwritten_excess"), a member known to the customer master is also kept to the group treaty's retention
/// on their life, less what the insurer already retains on it elsewhere (`retainedMinor`): a membership file
/// writes nothing over the retention without underwriting (`docs/reinsurance.md`, "Never silently
/// over-retained"). With "facultative" the excess is ceded and the product's limit alone applies.
function groupMember(category, salaryMinor, { excessPath = "facultative", retainedMinor = 0 } = {}) {
  const text = read("group_life.star");
  const c = new RegExp(`benefit_category\\("${category}", salary_multiple = "(\\d+)", max_sum_insured = dollars\\((\\d+)\\)`).exec(text);
  if (!c) throw new Error(`group_life.star has no category ${category}`);
  const limit = BigInt(/automatic_acceptance_limit = dollars\((\d+)\)/.exec(text)[1]) * 100n, cap = BigInt(c[2]) * 100n;
  const table = /group_unit_rate\("([^"]+)"\)/.exec(text)[1];
  const row = rateTable(table).find((r) => r.category === category);
  if (!row) throw new Error(`${table} has no rate for ${category}`);
  const byFormula = BigInt(salaryMinor) * BigInt(c[1]), formula = byFormula > cap ? cap : byFormula;
  let automatic = formula > limit ? limit : formula;
  if (excessPath === "underwritten_excess") {
    const free = groupRetention() - BigInt(retainedMinor);
    if (automatic > free) automatic = free > 0n ? free : 0n;
  }
  // cents a month = cover in cents / 1000 x rate / 12
  const monthly = halfEven(automatic * row.rate.n, 1000n * 12n * 10n ** BigInt(row.rate.scale));
  return { formula: Number(formula), automatic: Number(automatic), above: Number(formula - automatic), monthly: Number(monthly) };
}

/// The insurer's retention per life under the group life treaty (`treaties/group_life_surplus.star`), in cents.
function groupRetention() {
  const text = fs.readFileSync(path.resolve(ROOT, "..", "treaties", "group_life_surplus.star"), "utf8");
  const m = /basis = surplus\(retention = dollars\((\d+)\)/.exec(text);
  if (!m) throw new Error("group_life_surplus.star names no surplus retention");
  return BigInt(m[1]) * 100n;
}

const money = (cents) => (cents / 100).toFixed(2);
module.exports = { premium, age, addDays, addMonths, period, arrears, prices, product, rateTable, groupMember, money, halfEven, dec };
