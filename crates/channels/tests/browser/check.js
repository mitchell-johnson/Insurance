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

// With VIDEO set to a folder, each visit is recorded as a demonstration of
// the persona doing it, captioned, at a pace a person can follow.
const video = process.env.VIDEO;
const personas = {
  "/ -": "Anyone | The portal's front page",
  "/status -": "Anyone | Is the service up? The public status page",
  "/ops sub-ops": "ST16 Platform engineer | Region health and fail-over readiness",
  "/ops sub-kiri": "P18 Kiri Parata, customer | Refused the operations console",
  "/adviser sub-adv": "D1 Tessa Maddox, adviser | Commission, a disputed replacement, a client's policy, a message",
  "/employer sub-hr": "E1 Kai Fresh Supermarkets, scheme administrator | A payroll file, its preview and approval, a member",
  "/staff sub-fin1": "ST10 Finance operator | Held commission for a suspended adviser, maker and checker",
  "/staff sub-sue": "ST5 Service officer | Opens a policy with a case, enters a change for a customer who rang",
  "/staff sub-alice": "Staff administrator | Agrees a colleague's change above their limit",
  "/pricing sub-up1": "ST8 Unit pricing maker | A valuation, a price prepared, never self-approved",
  "/pricing sub-lead": "ST9 Unit pricing lead | Suspends pricing during an administrator outage",
  "/pricing sub-sue": "ST5 Service officer | Refused the unit pricing console",
  "/complaints sub-front": "Frontline staff | Records a complaint where it is heard",
  "/complaints sub-cmp1": "ST14 Complaints handler | Queue, assignment, the file, the register and report",
  "/complaints sub-con": "Conduct officer | Opens a conduct case",
  "/complaints sub-fin1": "ST10 Finance operator | Refused the complaints workbench",
  "/finance sub-gl1": "ST10 Finance operator | Month-end close checks and a journal for approval",
  "/finance sub-gl2": "Finance approver | Approves and posts the journal, the trial balance",
  "/finance sub-gllead": "ST11 Finance lead | Accounting groups and a refused cut-over total",
  "/finance sub-sue": "ST5 Service officer | Refused the finance console",
  "/payments sub-paycmp": "ST13 Compliance | Third-party payer cases",
  "/payments sub-paymk": "ST10 Finance operator | Prepares and checks a payment run",
  "/payments sub-payap": "ST17 Treasury approver | Bank reconciliation, approves the run",
  "/payments sub-gl1": "ST10 Finance operator | Refused payments without a payments role",
  "/group sub-grpview": "ST7 Group administrator (read only) | The exceptions queue",
  "/group sub-grp1": "ST7 Group administrator | Resolves a possible match, tries a file mapping",
  "/claims sub-cllead": "ST4 Senior claims assessor | Notifies a claim, works the queue, assigns it",
  "/claims sub-gl1": "ST10 Finance operator | Refused the claims console",
  "/reinsurance sub-ri1": "Reinsurance operations | Abandons a failed instruction, for a second signature",
  "/reinsurance sub-ri2": "Reinsurance lead | Signs and sends the abandonment",
  "/reinsurance sub-gl1": "ST10 Finance operator | Refused the reinsurance console",
  "/underwriting sub-amy": "ST1 Junior underwriter | Takes a case, proposes acceptance, reads disclosures",
  "/underwriting sub-sam": "ST2 Senior underwriter | Varies cover after non-disclosure, for a second senior",
  "/underwriting sub-cat": "ST2 Senior underwriter | Signs the second senior's part",
  "/underwriting sub-gl1": "ST10 Finance operator | Refused the underwriting console",
  "/adviser sub-fap": "D6 Harbourline Advice, financial advice provider | Its advisers and its own statement",
  "/replacements sub-wbcon": "Conduct officer | Decides disputed replacement matches",
  "/replacements sub-sue": "ST5 Service officer | Refused replacement decisions",
  "/commission sub-dadmin": "Distribution administrator | Asks the commission module, adds an adviser",
  "/commission sub-gl1": "ST10 Finance operator | Refused the commission console",
};
let clip = 0;
// Show a caption on the recording; nothing when not recording.
async function say(page, text, hold = 2500) {
  if (!video) return;
  await page.evaluate((t) => {
    let c = document.getElementById("demo-caption");
    if (!c) {
      c = document.createElement("div");
      c.id = "demo-caption";
      Object.assign(c.style, { position: "fixed", left: "16px", right: "16px", bottom: "16px", padding: "12px 16px", zIndex: 99999,
        background: "rgba(20,24,32,0.88)", color: "#fff", font: "600 18px/1.4 system-ui, sans-serif", borderRadius: "8px", pointerEvents: "none" });
      document.body.appendChild(c);
    }
    c.textContent = t;
  }, text);
  await page.waitForTimeout(hold);
}

async function visit(browser, path, who, steps) {
  const label = personas[`${path} ${who ? who.subject : "-"}`] || `${path} | ${who ? who.subject : "anyone"}`;
  const [persona, doing] = label.split(" | ");
  const name = `${String(++clip).padStart(2, "0")}-${persona.split(" ")[0]}-${path.replace(/\W/g, "") || "index"}`;
  const context = await browser.newContext(video ? { recordVideo: { dir: `${video}/${name}`, size: { width: 1280, height: 800 } }, viewport: { width: 1280, height: 800 } } : {});
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
    await say(page, `${persona}: ${doing}`, 3000);
    await steps.run(page);
    await say(page, `${persona}: ${doing}`, 2000);
  } catch (e) {
    problems.push(String(e.message).split("\n").slice(0, 3).join(" / "));
  }
  if (process.env.SHOTS) await page.screenshot({ path: `${process.env.SHOTS}/${path.replace(/\W/g, "") || "index"}.png`, fullPage: true });
  for (const p of problems) failures.push(`${path}: ${p}`);
  console.log(`${problems.length ? "FAIL" : "ok  "} ${path}`);
  await context.close();
  if (video) require("fs").writeFileSync(`${video}/${name}/caption.txt`, `${persona}\n${doing}\n`);
}
const sees = async (page, text) => {
  const found = page.getByText(text, { exact: false }).first();
  await found.waitFor({ timeout: 5000 });
  // On a recording, bring what is checked into view and let it be read.
  if (video) { await found.scrollIntoViewIfNeeded(); await page.waitForTimeout(700); }
};

(async () => {
  const browser = await chromium.launch(video ? { slowMo: 250 } : {});
  await visit(browser, "/", null, { run: async (p) => { await sees(p, "Adviser portal"); } });
  await visit(browser, "/status", null, { run: async (p) => { await sees(p, "Last checked"); await p.locator(".banner.ok").waitFor(); } });
  await visit(browser, "/ops", { subject: "sub-ops", roles: "operations" }, { run: async (p) => { await sees(p, "auckland"); await sees(p, "sydney"); } });
  await visit(browser, "/ops", { subject: "sub-kiri" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/adviser", { subject: "sub-adv" }, { expectStatus: [404], run: async (p) => {
    await say(p, "Her commission by provider, and what is held back");
    await sees(p, "fap-1");
    await sees(p, "$900.00");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
    await say(p, "A new policy was matched as replacing an old one. She disputes it, with a reason");
    await p.getByRole("button", { name: "Dispute" }).click();
    await p.fill("#d-reason", "new cover after a needs review");
    await p.locator("#dispute button").first().click();
    await sees(p, "Disputed. The commission");
    await say(p, "She opens a client's policy: the proposal, claims, units and documents");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await p.locator("#rows").getByText("prop-1").waitFor();
    await sees(p, "The applicant");
    await p.locator("#claims").getByText("No claims.").waitFor();
    // The schedule is listed; the documents module decides who may download it, and it is the customer's.
    await p.locator("#invest").getByText("No units held.").waitFor();
    await p.locator("#docs").getByText("Policy schedule").waitFor();
    await say(p, "The schedule is the customer's document: the adviser cannot download it");
    await p.locator("#docs").getByRole("button", { name: "Download" }).click();
    await p.locator("#docs-msg").getByText("not available to you").waitFor();
    await say(p, "She sends the insurer a note about the policy");
    await p.fill("#note", "customer asked by phone");
    await p.click("#send");
    await sees(p, "Sent");
  } });
  await visit(browser, "/employer", { subject: "sub-hr" }, { run: async (p) => {
    await say(p, "The administrator opens the scheme: members, cover, billing and claim status only");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await sees(p, "m-1");
    await p.locator("#claims").getByText("No claims.").waitFor();
    // The scheme on the group service: upload a file, see its preview, approve it, open a member's record.
    await p.locator("#group:not([hidden])").waitFor();
    await say(p, "This month's payroll file is uploaded and checked for viruses before anything reads it");
    await p.setInputFiles("#file", { name: "nov 2027.csv", mimeType: "text/csv", buffer: Buffer.from("employee_id,salary\nE001,80000\n") });
    await p.getByRole("button", { name: "Upload" }).click();
    await p.locator("#file-msg").getByText("Uploaded").waitFor();
    await say(p, "The preview shows who joins, leaves and changes");
    await p.getByRole("button", { name: "Show preview" }).click();
    await p.locator("#file-msg").getByText("quarantined").waitFor();
    await say(p, "The administrator approves the file");
    await p.locator("#approve-file").click();
    await p.locator("#file-msg").getByText("approved").waitFor();
    await say(p, "One member's record: cover and category, never their health loadings");
    await p.fill("#employee", "E001");
    await p.getByRole("button", { name: "Show", exact: true }).click();
    await p.locator("#member-view").getByText("staff").waitFor();
    if (/loading|exclusion/i.test(await p.locator("#member-view").innerText())) throw new Error("the employer saw a loading");
  } });
  await visit(browser, "/staff", { subject: "sub-fin1" }, { run: async (p) => {
    await say(p, "Commission held for an adviser whose licence is suspended");
    await p.getByRole("tab", { name: "Held balances" }).click();
    await sees(p, "adv-1");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#position:not([hidden])").waitFor();
    await sees(p, "suspended licence");
    await say(p, "She prepares a release, with the account, reason and evidence");
    await p.fill("#amt", "100.00"); await p.fill("#ref", "12-3456-7890123-00"); await p.fill("#reason", "REL-01"); await p.fill("#doc", "doc-1");
    await p.locator("#prepare button").click();
    await sees(p, "Prepared.");
    await sees(p, "awaiting approval");
    // The maker cannot approve their own action.
    await say(p, "She cannot approve her own action: someone else must");
    await p.getByRole("button", { name: "Approve", exact: true }).click();
    await p.locator("#bench-msg .banner.bad").waitFor();
    await say(p, "Her own access, and a declared personal connection she must not handle");
    await p.getByRole("tab", { name: "My access" }).click();
    await sees(p, "finance");
    await p.fill("#conn", "party-00000009"); await p.locator("#connect button").click();
    await sees(p, "Declared.");
    await p.locator("#connections").getByText("party-00000009").waitFor();
  }, expectStatus: [403, 409, 422] });
  let entered = null;
  await visit(browser, "/staff", { subject: "sub-sue" }, { run: async (p) => {
    await say(p, "A policy is opened only with the case being worked on, and the access is logged");
    await p.fill("#pol", process.env.POLICY || "missing"); await p.fill("#case", "case-1");
    await p.locator("#lookup button").click();
    await p.locator("#pol-view:not([hidden])").waitFor();
    // A change for a customer who rang, above her limit: it goes to a colleague first.
    await say(p, "A customer rang: she enters a change above her limit, so it needs a second person");
    await p.fill("#creason", "customer rang"); await p.fill("#camount", "900");
    await p.locator("#for-customer button").click();
    const said = await p.locator("#cust-msg").getByText("recorded").textContent();
    entered = said.match(/Request (\S+) recorded/)[1];
  } });
  await visit(browser, "/staff", { subject: "sub-alice" }, { run: async (p) => {
    await say(p, "A colleague agrees it; then it waits for the customer to approve in their app");
    await p.fill("#second-id", entered);
    await p.locator("#second button").click();
    await p.locator("#second-msg").getByText("Agreed.").waitFor();
  } });
  await visit(browser, "/pricing", { subject: "sub-up1" }, { run: async (p) => {
    await sees(p, "preparer, approver");
    await sees(p, "valuations/bad.csv");
    await say(p, "The fund administrator's valuation for the day");
    await p.selectOption("#pfund", "growth"); await p.fill("#pdate", "2027-11-12");
    await p.locator("#show-val").click();
    await sees(p, "1010000");
    await say(p, "The maker prepares the unit price from it");
    await p.locator("#prepare button").first().click();
    await sees(p, "Someone else approves it.");
    await p.locator("#work").getByText("growth:2027-11-12.1").waitFor();
    await p.getByRole("button", { name: "Details" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await sees(p, "units on issue");
    // The preparer cannot approve their own price.
    await say(p, "The maker cannot approve their own price");
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
    await say(p, "The queue: vulnerable complainants first, then the nearest deadline");
    await p.getByRole("tab", { name: "Queue" }).click();
    await p.locator("#queue").getByText("c-100").waitFor();
    await p.selectOption("#a-command", "assign");
    await p.fill("#f-0", "c-100"); await p.fill("#f-1", "cmp-1");
    await p.locator("#act button").click();
    await sees(p, "Done: assign.");
    await p.locator("#queue").getByRole("button", { name: "Open" }).first().click();
    await p.locator("#file:not([hidden])").waitFor();
    await sees(p, "doc-100");
    await say(p, "The complaints register and report for the month");
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
    await say(p, "The month-end close: what is stopping it");
    await p.fill("#p-id", "2027-11"); await p.locator("#period button").click();
    await p.locator("#dash:not([hidden])").waitFor();
    await sees(p, "Stopping the close");
    await p.selectOption("#r-check", "tax_reconciliation"); await p.locator("#report button").click();
    await sees(p, "Reported.");
    await say(p, "A manual journal with its reason and evidence, for someone else to approve");
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
    await say(p, "The approver posts it, and the trial balance shows it");
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
    await say(p, "A payroll row might be an existing person: staff decide the match");
    await p.fill("#rm-scheme", "harbour"); await p.fill("#rm-emp", "E002"); await p.fill("#rm-party", "party-x");
    await p.locator("#rm-form button").click();
    await p.locator("#rm-msg").getByText("Recorded.").waitFor();
    await p.locator("#ex-form button").click();
    await p.locator("#ex").getByText("Nothing is waiting.").waitFor();
    await say(p, "A new file layout is tried against the scheme's mapping, storing nothing");
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
    await p.locator("#inv-show").click();
    await p.locator("#inv").getByText("No cases are open to you.").waitFor();
    await say(p, "A trauma claim is notified");
    await p.selectOption("#a-command", "notify");
    await p.fill("#f-0", "c-b1"); await p.fill("#f-1", "pol-c1"); await p.fill("#f-2", "trauma");
    await p.selectOption("#f-4", "trauma"); await p.fill("#f-5", "2027-02-20");
    await p.locator("#act button").click();
    await sees(p, "Done: notify.");
    await sees(p, "c-b1: trauma");
    await say(p, "It appears in the lump sum queue and is assigned to an assessor");
    await p.selectOption("#q-name", "lump_sum"); await p.locator("#queue button").click();
    await p.locator("#q-rows").getByText("c-b1").waitFor();
    await p.fill("#a-purpose", "allocating lump sum work");
    await p.selectOption("#a-command", "assign"); await p.fill("#f-1", "cl-1");
    await p.locator("#act button").click();
    await sees(p, "Done: assign.");
    await p.locator("#c-summary").getByText("cl-1").waitFor();
    // The lead does not own the claim, so the claims module refuses their cause.
    await say(p, "Only the claim's owner records its cause: the lead is refused");
    await p.selectOption("#a-command", "record_cause"); await p.fill("#f-1", "CC-0003");
    await p.locator("#act button").click();
    await p.locator("#act-msg .banner.bad").waitFor();
  }, expectStatus: [403] });
  await visit(browser, "/reinsurance", { subject: "sub-ri1" }, { run: async (p) => {
    await sees(p, "you act on exceptions");
    await p.locator("#exceptions").getByText("delivery:1").waitFor();
    await say(p, "An instruction to the reinsurer failed. Abandoning it needs two people");
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
    await say(p, "She takes the next case from her queues");
    await takeFrom(p, ["medical", "financial", "occupational", "tele_interview"]);
    await say(p, "She proposes standard acceptance, within her authority");
    await p.selectOption("#c-what", "propose");
    await p.fill("#cf-cover", "life"); await p.selectOption("#cf-outcome", "accept");
    await p.fill("#cf-reasons", "RC-0101"); await p.fill("#cf-notes", "vault://notes");
    await p.locator("#c-form button").click();
    await p.locator("#c-msg").getByText("Done.").waitFor();
    await p.fill("#p-id", "P2"); await p.locator("#p-form button").first().click();
    await p.locator("#proposal").getByText("P2:").waitFor();
    await say(p, "The applicant's disclosures, read for a stated purpose");
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
    await say(p, "Non-disclosure found at claim: he varies the cover, and a second senior must sign");
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
    await say(p, "The maker prepares a provider payment run, adds an item and checks it");
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
    await say(p, "Treasury reconciles the bank account, then approves the run");
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
    // The group's own statement, with its overrides totalled.
    await p.fill("#st-number", "1"); await p.locator("#st-form button").click();
    await p.locator("#statement").getByText("Override (group)").waitFor();
  } });
  await visit(browser, "/replacements", { subject: "sub-wbcon" }, { run: async (p) => {
    await sees(p, "You decide disputed matches");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
  } });
  await visit(browser, "/commission", { subject: "sub-dadmin" }, { run: async (p) => {
    await sees(p, "distribution admin");
    await p.getByRole("button", { name: "Ask" }).click();
    await p.locator("#answer dl, #answer table, #answer span").first().waitFor();
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
