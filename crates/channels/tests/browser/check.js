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
    problems.push(String(e.message).split("\n").slice(0, 3).join(" / "));
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
  await visit(browser, "/adviser", { subject: "sub-adv" }, { expectStatus: [404], run: async (p) => {
    await sees(p, "fap-1");
    await sees(p, "$900.00");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
    await p.getByRole("button", { name: "Dispute" }).click();
    await p.fill("#d-reason", "new cover after a needs review");
    await p.locator("#dispute button").first().click();
    await sees(p, "Disputed. The commission");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await p.locator("#rows").getByText("prop-1").waitFor();
    await sees(p, "The applicant");
    await p.locator("#claims").getByText("No claims.").waitFor();
    // The schedule is listed; the documents module decides who may download it, and it is the customer's.
    await p.locator("#invest").getByText("No units held.").waitFor();
    await p.locator("#docs").getByText("Policy schedule").waitFor();
    await p.locator("#docs").getByRole("button", { name: "Download" }).click();
    await p.locator("#docs-msg").getByText("not available to you").waitFor();
    await p.fill("#note", "customer asked by phone");
    await p.click("#send");
    await sees(p, "Sent");
  } });
  await visit(browser, "/employer", { subject: "sub-hr" }, { run: async (p) => {
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await sees(p, "m-1");
    await p.locator("#claims").getByText("No claims.").waitFor();
    // The scheme on the group service: upload a file, see its preview, approve it, open a member's record.
    await p.locator("#group:not([hidden])").waitFor();
    await p.setInputFiles("#file", { name: "nov 2027.csv", mimeType: "text/csv", buffer: Buffer.from("employee_id,salary\nE001,80000\n") });
    await p.getByRole("button", { name: "Upload" }).click();
    await p.locator("#file-msg").getByText("Uploaded").waitFor();
    await p.getByRole("button", { name: "Show preview" }).click();
    await p.locator("#file-msg").getByText("quarantined").waitFor();
    await p.locator("#approve-file").click();
    await p.locator("#file-msg").getByText("approved").waitFor();
    await p.fill("#employee", "E001");
    await p.getByRole("button", { name: "Show", exact: true }).click();
    await p.locator("#member-view").getByText("staff").waitFor();
    if (/loading|exclusion/i.test(await p.locator("#member-view").innerText())) throw new Error("the employer saw a loading");
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
    await p.fill("#conn", "party-00000009"); await p.locator("#connect button").click();
    await sees(p, "Declared.");
    await p.locator("#connections").getByText("party-00000009").waitFor();
  }, expectStatus: [403, 409, 422] });
  await visit(browser, "/staff", { subject: "sub-sue" }, { run: async (p) => {
    await p.fill("#pol", process.env.POLICY || "missing"); await p.fill("#case", "case-1");
    await p.locator("#lookup button").click();
    await p.locator("#lookup-msg .banner.bad, #pol-view:not([hidden])").first().waitFor();
  }, expectStatus: [403, 404, 422] });
  await visit(browser, "/pricing", { subject: "sub-up1" }, { run: async (p) => {
    await sees(p, "preparer, approver");
    await sees(p, "valuations/bad.csv");
    await p.selectOption("#pfund", "growth"); await p.fill("#pdate", "2027-11-12");
    await p.locator("#show-val").click();
    await sees(p, "1010000");
    await p.locator("#prepare button").first().click();
    await sees(p, "Someone else approves it.");
    await p.locator("#work").getByText("growth:2027-11-12.1").waitFor();
    await p.getByRole("button", { name: "Details" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await sees(p, "units on issue");
    // The preparer cannot approve their own price.
    await p.getByRole("button", { name: "Approve", exact: true }).click();
    await p.locator("#work-msg .banner.bad").waitFor();
  }, expectStatus: [409] });
  await visit(browser, "/pricing", { subject: "sub-lead" }, { run: async (p) => {
    await sees(p, "lead");
    await p.selectOption("#sfund", "cash"); await p.fill("#sfrom", "2027-11-15"); await p.fill("#sreason", "administrator outage");
    await p.locator("#suspend button").click();
    await sees(p, "Suspended.");
    await sees(p, "administrator outage (lead-1)");
  } });
  await visit(browser, "/pricing", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/complaints", { subject: "sub-front" }, { run: async (p) => {
    await sees(p, "intake");
    await p.fill("#i-id", "c-100"); await p.fill("#i-who", "party-00000001"); await p.fill("#i-doc", "doc-100");
    await p.locator("#intake button").click();
    await sees(p, "Recorded c-100");
  } });
  await visit(browser, "/complaints", { subject: "sub-cmp1" }, { run: async (p) => {
    await sees(p, "trained to work with vulnerable customers");
    await p.getByRole("tab", { name: "Queue" }).click();
    await p.locator("#queue").getByText("c-100").waitFor();
    await p.selectOption("#a-command", "assign");
    await p.fill("#f-0", "c-100"); await p.fill("#f-1", "cmp-1");
    await p.locator("#act button").click();
    await sees(p, "Done: assign.");
    await p.locator("#queue").getByRole("button", { name: "Open" }).first().click();
    await p.locator("#file:not([hidden])").waitFor();
    await sees(p, "doc-100");
    await p.getByRole("tab", { name: "Register and report" }).click();
    await p.fill("#p-from", "2027-11-01"); await p.fill("#p-to", "2027-11-30");
    await p.locator("#period button").click();
    await p.locator("#register").getByText("c-100").waitFor();
    await p.getByRole("tab", { name: "Conduct cases" }).click();
    await p.locator("#case-msg .banner.bad").waitFor();
  } });
  await visit(browser, "/complaints", { subject: "sub-con" }, { run: async (p) => {
    await p.getByRole("tab", { name: "Queue" }).click();
    await p.selectOption("#a-command", "open_conduct_case");
    await p.fill("#f-0", "b-100"); await p.fill("#f-2", "con-1");
    await p.locator("#act button").click();
    await sees(p, "Done: open conduct case.");
    await p.getByRole("tab", { name: "Conduct cases" }).click();
    await p.locator("#cases").getByRole("button", { name: "Open" }).first().click();
    await p.locator("#case:not([hidden])").waitFor();
    await sees(p, "Owner con-1");
  } });
  await visit(browser, "/complaints", { subject: "sub-fin1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/finance", { subject: "sub-gl1" }, { run: async (p) => {
    await p.fill("#p-id", "2027-11"); await p.locator("#period button").click();
    await p.locator("#dash:not([hidden])").waitFor();
    await sees(p, "Stopping the close");
    await p.selectOption("#r-check", "tax_reconciliation"); await p.locator("#report button").click();
    await sees(p, "Reported.");
    await p.getByRole("tab", { name: "Journals" }).click();
    await p.fill("#j-id", "bj1"); await p.fill("#j-period", "2027-11"); await p.fill("#j-date", "2027-11-30");
    await p.fill("#j-reason", "month-end accrual"); await p.fill("#j-evidence", "vault://evidence/1");
    await p.fill("#j-lines", "6500, 250.00, SF1-life\n2500, -250.00, SF1-life");
    await p.locator("#journal button").click();
    await sees(p, "Someone else approves it.");
    await p.locator("#jview:not([hidden])").waitFor();
    // An operator holds no approver role, so the page does not offer approval.
    if (!(await p.locator("#j-approve").isDisabled())) throw new Error("an operator was offered approval");
  } });
  await visit(browser, "/finance", { subject: "sub-gl2" }, { run: async (p) => {
    await p.getByRole("tab", { name: "Journals" }).click();
    await p.fill("#jl-id", "bj1"); await p.locator("#jlookup button").click();
    await p.locator("#jview:not([hidden])").waitFor();
    await p.locator("#j-approve").click();
    await sees(p, "Approved and posted.");
    await p.getByRole("tab", { name: "Books" }).click();
    await p.fill("#tb-period", "2027-11"); await p.locator("#tb button").click();
    await p.locator("#tb-out").getByText("6500").first().waitFor();
  } });
  await visit(browser, "/payments", { subject: "sub-paycmp" }, { expectStatus: [409], run: async (p) => {
    // Compliance sees the payer cases; clearing one that is not open is refused in plain words.
    await p.locator("#aml").getByText("No open cases.").waitFor();
    await p.fill("#aml-case", "c1"); await p.fill("#aml-note", "a parent pays");
    await p.locator("#aml-form button").click();
    await p.locator("#aml-msg").getByText(/case/i).waitFor();
  } });
  await visit(browser, "/group", { subject: "sub-grpview" }, { run: async (p) => {
    // A reader sees the queue but is offered nothing to act with.
    await p.locator("#ex").getByText("possible match").waitFor();
    if (await p.locator("#rm-form").isVisible()) throw new Error("a reader was offered a decision");
  } });
  await visit(browser, "/group", { subject: "sub-grp1" }, { run: async (p) => {
    // Staff decide the possible match, and try a mapping without storing it.
    await p.locator("#ex").getByText("possible match").waitFor();
    await p.fill("#rm-scheme", "harbour"); await p.fill("#rm-emp", "E002"); await p.fill("#rm-party", "party-x");
    await p.locator("#rm-form button").click();
    await p.locator("#rm-msg").getByText("Recorded.").waitFor();
    await p.locator("#ex-form button").click();
    await p.locator("#ex").getByText("Nothing is waiting.").waitFor();
    await p.fill("#mp-scheme", "harbour");
    await p.getByRole("button", { name: "Show the mappings in use" }).click();
    await p.locator("#mp").getByText("harbour-freight").first().waitFor();
    await p.fill("#mp-sample", "Emp No\nE001\nE003\n");
    await p.locator("#mp-try").click();
    await p.locator("#mp-msg").getByText("Nothing was stored.").waitFor();
  } });
  await visit(browser, "/finance", { subject: "sub-gllead" }, { expectStatus: [409], run: async (p) => {
    // The lead sorts policies into accounting groups, and is told plainly why a cut-over total is refused.
    await p.getByRole("tab", { name: "Aged items and set-up" }).click();
    await p.locator("#ag-classify").click();
    await sees(p, "Classified.");
    await p.fill("#co-account", "1999"); await p.fill("#co-currency", "NZD");
    await p.fill("#co-total", "100000"); await p.fill("#co-evidence", "legacy TB");
    await p.locator("#co-send").click();
    await sees(p, "not a migration control account");
  } });
  await visit(browser, "/claims", { subject: "sub-cllead" }, { run: async (p) => {
    await sees(p, "You act as cl-lead");
    await p.selectOption("#a-command", "notify");
    await p.fill("#f-0", "c-b1"); await p.fill("#f-1", "pol-c1"); await p.fill("#f-2", "trauma");
    await p.selectOption("#f-4", "trauma"); await p.fill("#f-5", "2027-02-20");
    await p.locator("#act button").click();
    await sees(p, "Done: notify.");
    await sees(p, "c-b1: trauma");
    await p.selectOption("#q-name", "lump_sum"); await p.locator("#queue button").click();
    await p.locator("#q-rows").getByText("c-b1").waitFor();
    await p.fill("#a-purpose", "allocating lump sum work");
    await p.selectOption("#a-command", "assign"); await p.fill("#f-1", "cl-1");
    await p.locator("#act button").click();
    await sees(p, "Done: assign.");
    await p.locator("#c-summary").getByText("cl-1").waitFor();
    // The lead does not own the claim, so the claims module refuses their cause.
    await p.selectOption("#a-command", "record_cause"); await p.fill("#f-1", "CC-0003");
    await p.locator("#act button").click();
    await p.locator("#act-msg .banner.bad").waitFor();
  }, expectStatus: [403] });
  await visit(browser, "/reinsurance", { subject: "sub-ri1" }, { run: async (p) => {
    await sees(p, "you act on exceptions");
    await p.locator("#exceptions").getByText("delivery:1").waitFor();
    await p.getByRole("button", { name: "Abandon the instruction" }).click();
    await sees(p, "stay apart");
    await p.fill("#a-reason", "the policy was entered in error");
    await p.locator("#act-form button").first().click();
    await sees(p, "Someone else signs it");
    await p.getByRole("button", { name: "Withdraw" }).waitFor();
  } });
  await visit(browser, "/reinsurance", { subject: "sub-ri2" }, { run: async (p) => {
    await p.getByRole("button", { name: "Sign and send" }).click();
    await sees(p, "Signed and sent.");
    await sees(p, "No exceptions are open.");
    await sees(p, "Nothing is waiting.");
  } });
  // Find a case waiting in one of the queues and take it.
  const takeFrom = async (p, queues) => {
    for (const q of queues) {
      await p.selectOption("#q-name", q);
      await Promise.all([p.waitForResponse((r) => r.url().includes("/underwriting/queue?")), p.locator("#q-form button").click()]);
      await p.waitForTimeout(100);
      const take = p.locator("#queue").getByRole("button", { name: "Take" });
      if (await take.count()) { await take.first().click(); await sees(p, "You hold"); return; }
    }
    throw new Error("no case waiting in " + queues.join(", "));
  };
  await visit(browser, "/underwriting", { subject: "sub-amy" }, { run: async (p) => {
    await sees(p, "You are amy. You hold 0 open cases.");
    await takeFrom(p, ["medical", "financial", "occupational", "tele_interview"]);
    await p.selectOption("#c-what", "propose");
    await p.fill("#cf-cover", "life"); await p.selectOption("#cf-outcome", "accept");
    await p.fill("#cf-reasons", "RC-0101"); await p.fill("#cf-notes", "vault://notes");
    await p.locator("#c-form button").click();
    await p.locator("#c-msg").getByText("Done.").waitFor();
    await p.fill("#p-id", "P2"); await p.locator("#p-form button").first().click();
    await p.locator("#proposal").getByText("P2:").waitFor();
    await p.fill("#d-life", "L1"); await p.fill("#d-purpose", "assess the case"); await p.locator("#d-form button").click();
    await p.locator("#disclosures details").first().waitFor();
    await p.getByRole("button", { name: "Try again" }).click();
    await sees(p, "is back in the outbox");
    await sees(p, "Nothing is parked.");
    if (await p.locator("#senior").isVisible()) throw new Error("amy sees the senior's tools");
  } });
  await visit(browser, "/underwriting", { subject: "sub-sam" }, { run: async (p) => {
    await sees(p, "a senior underwriter");
    await takeFrom(p, ["senior_review"]);
    await p.fill("#n-review", "ndr/CLM-1/L1"); await p.selectOption("#n-outcome", "vary");
    await p.fill("#n-reasons", "RC-0980"); await p.fill("#n-cover", "life"); await p.fill("#n-loading", "50");
    await p.locator("#n-form button").click();
    await sees(p, "A second senior signs it");
    await p.getByRole("button", { name: "Withdraw" }).waitFor();
  } });
  await visit(browser, "/underwriting", { subject: "sub-cat" }, { run: async (p) => {
    await p.getByRole("button", { name: "Sign and send" }).click();
    await sees(p, "Signed and sent.");
    await sees(p, "Nothing is waiting.");
  } });
  await visit(browser, "/underwriting", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/payments", { subject: "sub-paymk" }, { run: async (p) => {
    await sees(p, "You are pay-mk: maker.");
    await p.fill("#r-id", "rb1"); await p.selectOption("#r-type", "provider_payments"); await p.fill("#r-date", "2027-11-16");
    await p.locator("#prep button").click();
    await sees(p, "Prepared. Add items");
    await p.locator("#run-form button").click();
    await sees(p, "provider payments from sf1");
    await p.fill("#raw-cmd", JSON.stringify({ type: "add_run_items", run: "rb1", items: [{ item: "ib1", source: "browser", counterparty: { payee: "p1" }, amount_minor: 1000, policy: null, payable: null, refund: null, represents: null }] }));
    await p.locator("#raw-form button").click();
    await p.locator("#raw-msg").getByText("Done.").waitFor();
    await p.locator("#run-form button").click();
    await p.locator("#run").getByText("ib1").waitFor();
    await p.getByRole("button", { name: "Check" }).click();
    await p.locator("#run-msg").getByText("Done: check run.").waitFor();
    if (await p.getByRole("button", { name: "Approve" }).count()) throw new Error("a maker is offered approval");
  } });
  await visit(browser, "/payments", { subject: "sub-payap" }, { run: async (p) => {
    await sees(p, "approver");
    await p.fill("#rec-account", "anz-sf1-claims"); await p.locator("#rec-form button").click();
    await sees(p, "closing 10000000.00");
    await p.locator("#runs").getByText("rb1").waitFor();
    await p.locator("#runs").getByRole("button", { name: "Open" }).first().click();
    await p.getByRole("button", { name: "Approve" }).click();
    await p.locator("#run-msg").getByText("Done: approve run.").waitFor();
    await p.fill("#party-id", "party-p1"); await p.locator("#party-form button").click();
    await p.locator("#party").getByText("p1").first().waitFor();
  } });
  await visit(browser, "/payments", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/adviser", { subject: "sub-fap" }, { run: async (p) => {
    await sees(p, "Advisers under you");
    await p.locator("#practice").getByText("adv-1").waitFor();
  } });
  await visit(browser, "/replacements", { subject: "sub-wbcon" }, { run: async (p) => {
    await sees(p, "You decide disputed matches");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
  } });
  await visit(browser, "/commission", { subject: "sub-dadmin" }, { run: async (p) => {
    await sees(p, "distribution admin");
    await p.getByRole("button", { name: "Ask" }).click();
    await p.locator("#answer pre").waitFor();
    await p.fill("#cmd", JSON.stringify({ add_party: { party: "adv-77", kind: "adviser" } }));
    await p.fill("#purpose", "onboarding");
    await p.getByRole("button", { name: "Send" }).click();
    await p.locator("#send-msg").getByText("Done.").waitFor();
  } });
  await visit(browser, "/commission", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/replacements", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/reinsurance", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/claims", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/finance", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await browser.close();
  if (failures.length) { console.error(failures.join("\n")); process.exit(1); }
  console.log("all pages check out");
})();
