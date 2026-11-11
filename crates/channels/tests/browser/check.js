// Drives the web pages in a real browser against the seeded portal.
//
//   cargo test -p ic-channels --test http serve_for_a_browser -- --ignored &
//   node crates/channels/tests/browser/check.js [http://127.0.0.1:8099]
//
// It fails on any script error, any content security policy violation, any
// failed request it did not expect, and any page that does not show what the
// seeded portal holds. Needs Playwright with Chromium.
const { chromium } = require("playwright");
const base = process.argv[2] || "http://127.0.0.1:8099";
const failures = [];

async function visit(browser, path, who, steps) {
  const context = await browser.newContext();
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push("script error: " + e.message));
  page.on("console", (m) => { if (m.type() === "error" && !m.text().startsWith("Failed to load resource")) problems.push("console: " + m.text()); });
  page.on("response", (r) => { if (r.status() >= 400 && !(steps.expectStatus || []).includes(r.status())) problems.push(`${r.status()} ${r.url()}`); });
  // Development sign-in asks for a subject and then for roles.
  const answers = who ? [who.subject, who.roles || ""] : [];
  page.on("dialog", (d) => d.accept(answers.length ? answers.shift() : "browser check"));
  try {
    await page.goto(base + path, { waitUntil: "networkidle" });
    await steps.run(page);
  } catch (e) {
    problems.push(String(e.message).split("\n")[0]);
  }
  if (process.env.SHOTS) await page.screenshot({ path: `${process.env.SHOTS}/${path.replace(/\W/g, "") || "index"}.png`, fullPage: true });
  for (const p of problems) failures.push(`${path}: ${p}`);
  console.log(`${problems.length ? "FAIL" : "ok  "} ${path}`);
  await context.close();
}
const sees = (page, text) => page.getByText(text, { exact: false }).first().waitFor({ timeout: 5000 });

(async () => {
  const browser = await chromium.launch();
  await visit(browser, "/", null, { run: async (p) => { await sees(p, "Adviser portal"); } });
  await visit(browser, "/status", null, { run: async (p) => { await sees(p, "Last checked"); await p.locator(".banner.ok").waitFor(); } });
  await visit(browser, "/ops", { subject: "sub-ops", roles: "operations" }, { run: async (p) => { await sees(p, "auckland"); await sees(p, "sydney"); } });
  await visit(browser, "/ops", { subject: "sub-kiri" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/adviser", { subject: "sub-adv" }, { run: async (p) => {
    await sees(p, "fap-1");
    await sees(p, "$900.00");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await p.fill("#note", "customer asked by phone");
    await p.click("#send");
    await sees(p, "Sent");
  } });
  await visit(browser, "/employer", { subject: "sub-hr" }, { run: async (p) => {
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await sees(p, "m-1");
  } });
  await visit(browser, "/staff", { subject: "sub-fin1" }, { run: async (p) => {
    await p.getByRole("tab", { name: "Held balances" }).click();
    await sees(p, "adv-1");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#position:not([hidden])").waitFor();
    await sees(p, "suspended licence");
    await p.fill("#amt", "100.00"); await p.fill("#ref", "12-3456-7890123-00"); await p.fill("#reason", "REL-01"); await p.fill("#doc", "doc-1");
    await p.locator("#prepare button").click();
    await sees(p, "Prepared.");
    await sees(p, "awaiting approval");
    // The maker cannot approve their own action.
    await p.getByRole("button", { name: "Approve", exact: true }).click();
    await p.locator("#bench-msg .banner.bad").waitFor();
    await p.getByRole("tab", { name: "My access" }).click();
    await sees(p, "finance");
  }, expectStatus: [403, 409, 422] });
  await visit(browser, "/staff", { subject: "sub-sue" }, { run: async (p) => {
    await p.fill("#pol", process.env.POLICY || "missing"); await p.fill("#case", "case-1");
    await p.locator("#lookup button").click();
    await p.locator("#lookup-msg .banner.bad, #pol-view:not([hidden])").first().waitFor();
  }, expectStatus: [403, 404, 422] });
  await browser.close();
  if (failures.length) { console.error(failures.join("\n")); process.exit(1); }
  console.log("all pages check out");
})();
