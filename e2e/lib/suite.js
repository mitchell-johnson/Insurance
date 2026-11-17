// The persona suite's harness: how a test is declared, how a persona opens a
// screen, and what counts as a failure.
//
// It keeps the rules of the portal's own browser check
// (`crates/channels/tests/browser/check.js`): a test fails on any script
// error, any console error (which is how a content security policy violation
// shows), any browser dialog, any response of 400 or above that the test did
// not say it expected, and anything the screen was meant to show and did
// not. Signing in is the development sign-in card, filled as a person would.
//
// A test is one persona journey through the screens of the running system.
// It declares who it is for and why it exists; `tools/check_test_docs.py`
// holds Rust tests to the same two lines.
"use strict";
const { execFile } = require("child_process");
const fs = require("fs");
const path = require("path");
const { System } = require("../stack/cluster");

const tests = [];
const REQUIRED = ["id", "area", "persona", "story", "title", "purpose", "why"];

/// Declare a test. `meta.matrix` lists the persona matrix rows it carries
/// out at the screen; `meta.lock` names state it must not share with a test
/// running beside it (one finance period, one payment run).
function test(meta, run) {
  for (const key of REQUIRED) if (!meta[key]) throw new Error(`test ${meta.id || meta.title || "?"} has no ${key}`);
  if (tests.some((t) => t.meta.id === meta.id)) throw new Error(`two tests are called ${meta.id}`);
  tests.push({ meta: { matrix: [], ...meta }, run });
}

class Screen {
  constructor(t, page, who) { this.t = t; this.page = page; this.who = who; this.allowed = new Set(); this.refusals = []; }
  $(selector) { return typeof selector === "string" ? this.page.locator(selector) : selector; }
  /// Move to one of the page's views by its tab.
  async tab(name) { await this.page.getByRole("tab", { name, exact: true }).click(); }
  /// Wait for words to be on screen, anywhere or inside `within`.
  async sees(text, within, timeout = 10000) {
    const scope = within ? this.$(within) : this.page;
    await scope.getByText(text, { exact: false }).first().waitFor({ state: "visible", timeout });
  }
  /// The words must not be on screen (after the page has settled).
  async never(text, within) {
    await this.page.waitForLoadState("networkidle");
    const scope = within ? this.$(within) : this.page;
    const n = await scope.getByText(text, { exact: false }).filter({ visible: true }).count();
    if (n) throw new Error(`"${text}" is on screen and must not be`);
  }
  async fill(selector, value) { await this.$(selector).fill(String(value)); }
  /// Pick from a field's list as a person does: type what they know, then press the entry that reads `label`.
  async choose(selector, text, label = text) {
    const input = this.$(selector);
    await input.fill(String(text));
    const entry = input.locator("xpath=..").locator("ul.picks li").filter({ hasText: label }).first();
    try { await entry.waitFor({ state: "visible", timeout: 10000 }); }
    catch { const note = await input.locator("xpath=..").locator(".pick-note").innerText().catch(() => ""); throw new Error(`"${text}" offered no "${label}" to pick${note ? `; the field said "${note}"` : ""}`); }
    await entry.click();
  }
  async pick(selector, value) { await this.$(selector).selectOption(value); }
  /// Banners already on screen are old news once the person acts again: `outcome` waits for a new one.
  async stale() { await this.page.evaluate(() => { for (const b of document.querySelectorAll(".banner")) b.dataset.seen = "1"; }); }
  async click(selector) { await this.stale(); await this.$(selector).click(); }
  async press(name, within) { await this.stale(); await (within ? this.$(within) : this.page).getByRole("button", { name, exact: true }).first().click(); }
  /// The outcome banner an action leaves in `selector`: "ok" or "bad", with its words.
  async outcome(selector, timeout = 15000) {
    const banner = this.$(selector).locator(".banner:not([data-seen])").first();
    await banner.waitFor({ state: "visible", timeout });
    const kind = (await banner.getAttribute("class")).includes("bad") ? "bad" : (await banner.getAttribute("class")).includes("ok") ? "ok" : "info";
    return { kind, words: (await banner.innerText()).trim() };
  }
  /// The action must succeed, and say so.
  async done(selector, words) {
    const o = await this.outcome(selector);
    if (o.kind === "bad") throw new Error(`the screen refused: ${o.words}`);
    if (words && !o.words.includes(words)) throw new Error(`expected "${words}", the screen said "${o.words}"`);
    return o.words;
  }
  /// The system must stop the person, with these words, answering one of `statuses`.
  async stopped(selector, words, ...statuses) {
    for (const s of statuses) this.allowed.add(s);
    const o = await this.outcome(selector);
    if (o.kind !== "bad") throw new Error(`expected a refusal, the screen said "${o.words}"`);
    if (words && !o.words.toLowerCase().includes(String(words).toLowerCase())) throw new Error(`expected a refusal saying "${words}", the screen said "${o.words}"`);
    return o.words;
  }
  /// Responses of these statuses are part of the journey from here on.
  expect(...statuses) { for (const s of statuses) this.allowed.add(s); }
  /// The whole page is refused: only the refusal shows.
  async refused(words) {
    this.allowed.add(403);
    await this.page.locator(".gate.refused").waitFor({ state: "visible", timeout: 10000 });
    if (words) await this.sees(words, ".gate.refused");
  }
  /// Answer a question the page asks in its own dialog.
  async answer(text) { await this.page.fill("dialog[open] input", text); await this.page.click("dialog[open] .actions button:last-child"); }
  /// The text of a table's rows under `selector`, as arrays of cells.
  async rows(selector) {
    return this.$(selector).locator("tbody tr").evaluateAll((trs) => trs.map((tr) => [...tr.cells].map((c) => c.innerText.trim())));
  }
  /// The row of a table that mentions `text`.
  row(selector, text) { return this.$(selector).locator("tbody tr").filter({ hasText: text }).first(); }
  /// Fill the generic action form the consoles share: pick the action, then each field by its label
  /// (a value `{ pick, label }` is picked from the field's list, see `choose`).
  async act(form, action, fields = {}, { purpose } = {}) {
    const f = this.$(form);
    if (purpose !== undefined) await f.locator("#a-purpose").fill(purpose);
    await f.locator("#a-command").selectOption(action);
    for (const [label, value] of Object.entries(fields)) {
      // The field whose label reads exactly this (a label's words may hold brackets and quotes).
      const id = await f.locator("label").evaluateAll((labels, words) => { const l = labels.find((x) => x.textContent.trim() === words); return l ? l.getAttribute("for") : null; }, label);
      if (!id) throw new Error(`the form for "${action}" has no field labelled "${label}"`);
      const input = f.locator("#" + id);
      // `{ pick: "what is typed", label: "the entry pressed" }` picks from the field's list as a person does.
      if (value && typeof value === "object") { await this.choose(input, value.pick, value.label); continue; }
      const tag = await input.evaluate((el) => el.tagName);
      if (tag === "SELECT") await input.selectOption(String(value)); else await input.fill(String(value));
    }
    await this.stale();
    await f.getByRole("button", { name: "Send", exact: true }).click();
  }
  async text(selector) { return (await this.$(selector).innerText()).trim(); }
  async wait(ms) { await this.page.waitForTimeout(ms); }
}

class Journey {
  constructor(meta, browser, stack, options) {
    this.meta = meta; this.browser = browser; this.stack = stack; this.options = options;
    this.problems = []; this.screens = []; this.contexts = []; this.steps = [];
    this.sys = new System(stack);
  }
  /// A reference no other test, attempt or run uses: `CMP-<run>-<test>-<n>`.
  ref(prefix) { this.n = (this.n || 0) + 1; return `${prefix}-${this.options.run}${this.options.attempt > 1 ? "r" : ""}-${this.meta.id.replace(/\D/g, "")}${this.n > 1 ? "-" + this.n : ""}`; }
  /// Issue a policy from one of the shipped application templates (`term_life`, `income_protection`,
  /// `health_family`, `invest_plan`), as new business does, and wait until the portal knows it.
  /// It starts a fortnight before the business date unless `over.inception` says otherwise.
  async policy(kind, party, over = {}) {
    const shipped = JSON.parse(fs.readFileSync(path.join(this.stack.run, "shipped.json"), "utf8"));
    const a = JSON.parse(JSON.stringify(shipped.applications[kind]));
    const id = this.ref("pol").toLowerCase();
    Object.assign(a, { policy: id, number: id.toUpperCase(), group: `grp-${id}`, policyholder: party, inception: Journey.before(this.stack.opening, 14) });
    for (const life of a.lives) if (life.kind.party) life.kind.party = party;
    Object.assign(a, over);
    const r = await this.sys.must(this.sys.api("POST", "/v1/policies", a), `issue ${kind}`);
    if (r.status !== "applied") throw new Error(`issue ${kind}: ${JSON.stringify(r)}`);
    for (let n = 0; n < 50; n++) {
      const seen = await this.sys.portal("GET", `/staff/v1/policies/${id}?case=issue`, { subject: "sub-st5b" });
      if (seen.ok && seen.data.found) return { id, number: a.number, partition: r.partition, application: a };
      await new Promise((res) => setTimeout(res, 200));
    }
    throw new Error(`the portal never heard of policy ${id}`);
  }
  /// The date `days` before (or, negative, after) a date.
  static before(date, days) { return new Date(Date.parse(date + "T00:00:00Z") - days * 86400000).toISOString().slice(0, 10); }
  /// Do something through the portal's own routes as a persona, without a screen: the set-up a journey starts from.
  async does(who, method, pathname, body, what) { return this.sys.must(this.sys.portal(method, pathname, who, body), `${who.key} ${what || pathname}`); }
  note(text) { this.steps.push(text); }

  // --- Time and money, for the lifecycle journeys (`lock: "business-date"`; they run after every other test). ---
  /// The ledger's business date now.
  async today() {
    const r = await this.sys.must(this.sys.http("GET", this.stack.api, "/v1/admin/reconciliation", { headers: { "x-dev-subject": "sub-st16", "x-dev-roles": "operations" } }), "read the business date");
    return r.business_date;
  }
  /// Close `n` business days: the end-of-day close the scheduled workflow runs, once per day, by the
  /// `ic-e2e-close` tool. Each close must reconcile; a break fails the journey.
  async closes(n = 1) {
    const bin = path.resolve(__dirname, "../..", process.env.IC_BIN || "target/debug", "ic-e2e-close");
    const env = { PATH: process.env.PATH, IC_NODES: this.stack.nodes.map((u) => u.replace(/^https?:\/\//, "")).join(","), IC_CLUSTER_TOKEN: this.stack.token, IC_ROOT_DIR: path.join(this.stack.run, "roots") };
    let last = null;
    for (let i = 0; i < n; i++) {
      const out = await new Promise((done, fail) => execFile(bin, [], { env, maxBuffer: 16 * 1024 * 1024 }, (e, stdout, stderr) => (e ? fail(new Error(`the close failed: ${stderr || e.message}`)) : done(stdout))));
      last = JSON.parse(out);
      if (last.breaks && last.breaks.length) throw new Error(`the close of ${last.closed} did not reconcile: ${JSON.stringify(last.breaks).slice(0, 300)}`);
    }
    return last;
  }
  /// Close days until the business date is `date`.
  async closesTo(date) {
    for (let n = 0; n < 800; n++) { if ((await this.today()) >= date) return; await this.closes(1); }
    throw new Error(`the business date never reached ${date}`);
  }
  /// Money arrives at the bank quoting a policy number, and cash matching applies it.
  async pays(number, minor) {
    const id = this.ref("rc").toLowerCase();
    const r = await this.sys.must(this.sys.http("POST", this.stack.api, "/v1/payments", { headers: { "x-dev-subject": "svc-cash-matching", "x-dev-roles": "cash-matching", "x-caller": "cash-matching" },
      body: { id, amount: { minor, currency: "NZD" }, reference: number, payer_account: null, received_on: await this.today() } }), `receipt for ${number}`);
    if (r.status !== "applied") throw new Error(`the receipt for ${number} was not applied: ${JSON.stringify(r)}`);
    return id;
  }
  /// A command to the core from one of its named calling services (policy servicing, collections).
  core(caller, command, policy) {
    return this.sys.http("POST", this.stack.api, "/v1/commands", { headers: { "x-dev-subject": `svc-${caller}`, "x-dev-roles": caller, "x-caller": caller, "idempotency-key": this.ref("cmd") }, body: { command, policy } });
  }
  /// Open a page as a persona. `who` is a member of the cast (`lib/cast.js`), or null for a public page.
  /// `statuses` are refusals the page is expected to meet while it loads.
  async open(pathname, who, ...statuses) {
    const look = { colorScheme: this.options.theme || "light", viewport: { width: this.options.width || 1280, height: 800 } };
    if (this.options.video) look.recordVideo = { dir: path.join(this.options.video, this.meta.id), size: { width: 1280, height: 800 } };
    const context = await this.browser.newContext(look);
    this.contexts.push(context);
    const page = await context.newPage();
    const screen = new Screen(this, page, who);
    screen.expect(...statuses);
    const tag = `${pathname}${who ? " as " + who.subject : ""}`;
    page.on("pageerror", (e) => this.problems.push(`${tag}: script error: ${e.message}`));
    page.on("console", (m) => { if (m.type() === "error" && !m.text().startsWith("Failed to load resource")) this.problems.push(`${tag}: console: ${m.text()}`); });
    // A refusal is judged when the test ends, so a test may say it expected one after it has seen it.
    page.on("response", (r) => { if (r.status() >= 400) screen.refusals.push([r.status(), `${tag}: ${r.status()} ${r.request().method()} ${new URL(r.url()).pathname}`]); });
    page.on("dialog", (d) => { this.problems.push(`${tag}: browser dialog: ${d.message()}`); d.dismiss().catch(() => {}); });
    this.screens.push(screen);
    await page.goto(this.stack.portal + pathname, { waitUntil: "networkidle" });
    if (who) {
      await page.fill("#dev-subject", who.subject);
      await page.fill("#dev-roles", who.roles || "");
      await page.click("#dev-sign-in button");
      await page.waitForLoadState("networkidle");
    }
    return screen;
  }
  /// Everything that went wrong: what the pages reported, and each refusal no step expected.
  findings() { return [...this.problems, ...this.screens.flatMap((s) => s.refusals.filter(([status]) => !s.allowed.has(status)).map(([, line]) => line))]; }
  async close(failed) {
    if (failed && this.options.shots) {
      fs.mkdirSync(this.options.shots, { recursive: true });
      for (const [i, s] of this.screens.entries()) await s.page.screenshot({ path: path.join(this.options.shots, `${this.meta.id}-${i + 1}.png`), fullPage: true, timeout: 10000 }).catch(() => {});
    }
    for (const c of this.contexts) await c.close().catch(() => {});
  }
}

/// Money as the consoles show it, in the browser's currency format for New Zealand dollars: NZ$1,234.56.
const nz = (minor) => (minor < 0 ? "-" : "") + "NZ$" + (Math.abs(minor) / 100).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
/// An amount read off a console, as a person types it back into a form: 1234.56.
const typed = (shown) => shown.replace(/NZ\$/, "").replace(/,/g, "");

module.exports = { test, tests, Journey, Screen, nz, typed };
