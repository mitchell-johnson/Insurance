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
  "/customer sub-kiri": "P18 Kiri Parata, customer | Her policy and bill, sets up this browser to sign, approves her adviser's changes, her details",
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
  "/collections sub-colof": "Collections officer | Today's queue and the call tasks",
  "/collections sub-colcfg": "Collections configuration | Sets a product's dunning sequence from the shipped best estimate",
  "/collections sub-gl1": "ST10 Finance operator | Refused the collections console",
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
  if (process.env.ONLY && process.env.ONLY !== path) return;
  const label = personas[`${path} ${who ? who.subject : "-"}`] || `${path} | ${who ? who.subject : "anyone"}`;
  const [persona, doing] = label.split(" | ");
  const name = `${String(++clip).padStart(2, "0")}-${persona.split(" ")[0]}-${path.replace(/\W/g, "") || "index"}`;
  // THEME=dark and WIDTH=390 look at the pages in the dark theme and at phone width.
  const look = { colorScheme: process.env.THEME === "dark" ? "dark" : "light", viewport: { width: Number(process.env.WIDTH) || 1280, height: 800 } };
  const context = await browser.newContext(video ? { ...look, recordVideo: { dir: `${video}/${name}`, size: { width: 1280, height: 800 } } } : look);
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push("script error: " + e.message));
  page.on("console", (m) => { if (m.type() === "error" && !m.text().startsWith("Failed to load resource")) problems.push("console: " + m.text()); });
  page.on("response", (r) => { if (r.status() >= 400 && !(steps.expectStatus || []).includes(r.status())) problems.push(`${r.status()} ${r.url()}`); });
  // The pages ask their own questions; a browser dialog is a fault.
  page.on("dialog", (d) => { problems.push("browser dialog: " + d.message()); d.dismiss(); });
  try {
    await page.goto(base + path, { waitUntil: "networkidle" });
    // Development sign-in: the page asks who the person is before it shows anything.
    if (who) {
      await page.fill("#dev-subject", who.subject);
      await page.fill("#dev-roles", who.roles || "");
      await page.click("#dev-sign-in button");
      await page.waitForLoadState("networkidle");
    }
    await say(page, `${persona}: ${doing}`, 3000);
    await steps.run(page);
    await say(page, `${persona}: ${doing}`, 2000);
    // What a screen reader needs of every page, in every view, shown or not (persona row AUD-018).
    for (const fault of await page.evaluate(audit)) problems.push("accessibility: " + fault);
  } catch (e) {
    problems.push(String(e.message).split("\n").slice(0, 3).join(" / "));
  }
  if (process.env.SHOTS) await page.screenshot({ timeout: 10000, path: `${process.env.SHOTS}/${path.replace(/\W/g, "") || "index"}-${who ? who.subject : "anyone"}.png`, fullPage: true }).catch((e) => problems.push("screenshot: " + e.message.split("\n")[0]));
  for (const p of problems) failures.push(`${path}: ${p}`);
  console.log(`${problems.length ? "FAIL" : "ok  "} ${path}`);
  await context.close();
  if (video) require("fs").writeFileSync(`${video}/${name}/caption.txt`, `${persona}\n${doing}\n`);
}
// Runs in the page. Every control has a name a screen reader can say, every table has headers,
// the page has a language, a title, one main heading and a main region, and no ID is used twice.
function audit() {
  const faults = [];
  const where = (el) => `<${el.tagName.toLowerCase()}${el.id ? ` id="${el.id}"` : ""}${el.name ? ` name="${el.name}"` : ""}>`;
  const named = (el) => (el.getAttribute("aria-label") || "").trim() || (el.getAttribute("aria-labelledby") || "").split(/\s+/).some((id) => id && document.getElementById(id) && document.getElementById(id).textContent.trim());
  for (const el of document.querySelectorAll("input, select, textarea")) {
    if (el.type === "hidden" || el.closest("#demo-bar")) continue;
    const label = (el.id && document.querySelector(`label[for="${CSS.escape(el.id)}"]`)) || el.closest("label");
    if (!(label && label.textContent.trim()) && !named(el)) faults.push(`${where(el)} has no label`);
  }
  for (const el of document.querySelectorAll("button, a[href], [role=tab]")) {
    if (el.closest("#demo-bar")) continue;
    if (!el.textContent.trim() && !named(el) && !(el.getAttribute("title") || "").trim()) faults.push(`${where(el)} has no name`);
  }
  for (const t of document.querySelectorAll("table")) if (t.rows.length && !t.querySelector("th")) faults.push("a table has no header cells");
  for (const svg of document.querySelectorAll("svg")) if (svg.getAttribute("aria-hidden") !== "true" && !named(svg)) faults.push("an icon is neither named nor hidden from the accessibility tree");
  const ids = [...document.querySelectorAll("[id]")].map((el) => el.id);
  for (const id of new Set(ids.filter((id, i) => ids.indexOf(id) !== i))) faults.push(`id "${id}" is used more than once`);
  if (!document.documentElement.lang) faults.push("the page names no language");
  if (!document.title.trim()) faults.push("the page has no title");
  if (document.querySelectorAll("h1").length !== 1) faults.push(`the page has ${document.querySelectorAll("h1").length} main headings, not one`);
  if (!document.querySelector("main")) faults.push("the page has no main region");
  for (const live of document.querySelectorAll("[role=alert], .banner.bad")) if (!live.closest("[aria-live], [role=alert]")) faults.push("a refusal is not announced: it sits outside a live region");
  return [...new Set(faults)];
}
// Move to one of a page's views by its tab.
const tab = (page, name) => page.getByRole("tab", { name, exact: true }).click();
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
  // The customer reads before she can sign: an approval fails until this browser is set up, then her phone approves the browser.
  await visit(browser, "/customer", { subject: "sub-kiri" }, { run: async (p) => {
    await sees(p, "2 changes are waiting for you to approve");
    await p.locator("#covers table").waitFor();
    await sees(p, "$500,000.00");
    await sees(p, "$61.84 is due");
    await sees(p, "Policy schedule");
    await tab(p, "To approve");
    await sees(p, "decline indexation");
    await p.click("#approve-req-cover");
    await sees(p, "Approving needs your signature");
    await tab(p, "Signing");
    await say(p, "She sets up this browser: the key is made here and never leaves it");
    await p.click("#sg-make");
    await sees(p, "Approve it from your phone to finish");
    const device = await p.textContent("#sg-device"), key = await p.textContent("#sg-key");
    // What her phone app does: it signs the new browser's key and sends it.
    const signature = await (await fetch(base + "/test/v1/phone-signs", { method: "POST", body: `enrol\n${device}\n${key}` })).text();
    const enrol = await fetch(base + "/app/v1/devices", { method: "POST", headers: { "content-type": "application/json", "x-dev-subject": "sub-kiri", "idempotency-key": "enrol-" + device },
      body: JSON.stringify({ device, public_key: key, hardware_backed: false, approver: "phone", signature }) });
    if (!enrol.ok) throw new Error("the phone could not approve the browser: " + enrol.status);
    await p.click("#sg-check");
    await sees(p, "This browser can sign for you");
    await tab(p, "To approve");
    await say(p, "Now each approval is signed by this browser");
    await p.click("#approve-req-money");
    await sees(p, "Approved: withdrawal");
    await p.click("#decline-req-cover");
    await p.click("dialog button.danger");
    await sees(p, "Declined: decline indexation");
    await sees(p, "Nothing is waiting for you");
    await tab(p, "Your details");
    await sees(p, "kiri@example.nz");
    await p.click("#hold-on");
    await sees(p, "A hold is on");
    await tab(p, "Your policies");
    await say(p, "She tells us about a claim: which cover, what happened and when");
    await p.selectOption("#nc-type", "death");
    await p.fill("#nc-date", "2027-02-20");
    await p.click("#notify button");
    await sees(p, "The claims team will be in touch");
    await p.locator("#claims").getByText(/^app-/).waitFor();
  } });
  await visit(browser, "/adviser", { subject: "sub-adv" }, { expectStatus: [404], run: async (p) => {
    await say(p, "Her commission by provider, and what is held back");
    await tab(p, "Engagements");
    await sees(p, "fap-1");
    await tab(p, "Commission");
    await sees(p, "$900.00");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
    await say(p, "A new policy was matched as replacing an old one. She disputes it, with a reason");
    await p.getByRole("button", { name: "Dispute" }).click();
    await p.fill("#d-reason", "new cover after a needs review");
    await p.locator("#dispute button").first().click();
    await sees(p, "Disputed. The commission");
    await say(p, "She opens a client's policy: the proposal, claims, units and documents");
    await tab(p, "Your book");
    await p.getByRole("button", { name: "Open" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    await p.locator("#rows").getByText("prop-1").waitFor();
    await sees(p, "The applicant");
    // Kiri notified a claim from her own page earlier in this run: her adviser sees its status.
    await p.locator("#claims").getByText(/^No claims\.$|^open$/).first().waitFor();
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
    await tab(p, "Files and members");
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
  await visit(browser, "/staff", { subject: "sub-hcs" }, { run: async (p) => {
    await say(p, "A person trained in vulnerability support reviews the markers that are due");
    await p.getByRole("tab", { name: "Markers to review" }).click();
    await p.locator("#care-list").getByText("review due").waitFor();
    await sees(p, "death of partner");
    await say(p, "She keeps one marker and sets when it is next reviewed");
    await p.locator("#care-list").getByRole("button", { name: "Decide" }).first().click();
    await p.locator("#care-one:not([hidden])").waitFor();
    await sees(p, "Marker now: bereavement");
    await p.fill("#care-date", "2028-05-01");
    await p.locator("#care-form button").click();
    await p.locator("#care-msg").getByText("Recorded.").waitFor();
    await say(p, "A prompt after a death is closed with no marker needed");
    await p.locator("#care-list").getByRole("button", { name: "Decide" }).first().click();
    await p.selectOption("#care-outcome", "no_marker");
    if (await p.locator("#care-adj-box").isVisible()) throw new Error("adjustments were asked for with no marker");
    await p.locator("#care-form button").click();
    await p.locator("#care-list").getByText("Nothing to review").waitFor();
  } });
  await visit(browser, "/staff", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => {
    await say(p, "An officer without the training is not shown who is marked");
    await p.getByRole("tab", { name: "Markers to review" }).click();
    await p.locator("#care-list").getByText("your role does not include it").waitFor();
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
    await p.locator("dialog[open] .actions button:last-child").click();
    await sees(p, "Declared.");
    await p.locator("#connections").getByText("party-00000009").waitFor();
  }, expectStatus: [403, 409, 422] });
  let entered = null;
  await visit(browser, "/staff", { subject: "sub-sue" }, { run: async (p) => {
    await say(p, "A customer rings: she starts a call reference and finds them by name and birth date");
    await p.locator("#new-call").click();
    await p.fill("#find-q", "Kiri Parata 12/4/1988"); await p.locator("#find button:not([type])").click();
    await p.locator("#found").getByText("Kiri Parata").first().waitFor();
    await p.locator("#found").getByRole("button", { name: "Open" }).first().click();
    await p.locator("#pol-view:not([hidden])").waitFor();
    await say(p, "Someone phones about the policy and cannot be identified: she is given words that confirm nothing");
    await p.locator("#caller-form button").click();
    await p.locator("#caller-msg").getByText("can't confirm whether we hold a policy").waitFor();
    // A change for a customer who rang, above her limit: it goes to a colleague first.
    await say(p, "A customer rang: she enters a change above her limit, so it needs a second person");
    await p.fill("#creason", "customer rang"); await p.fill("#camount", "900");
    await p.locator("#for-customer button").click();
    const said = await p.locator("#cust-msg").getByText("recorded").textContent();
    entered = said.match(/Request (\S+) recorded/)[1];
  } });
  await visit(browser, "/staff", { subject: "sub-alice" }, { run: async (p) => {
    await say(p, "A colleague agrees it; then it waits for the customer to approve in their app");
    await p.getByRole("tab", { name: "Second approvals" }).click();
    await p.locator("#waiting tbody tr").filter({ hasText: entered }).getByRole("button", { name: "Agree", exact: true }).click();
    await p.locator("#second-msg").getByText("Agreed.").waitFor();
  } });
  await visit(browser, "/pricing", { subject: "sub-up1" }, { run: async (p) => {
    await sees(p, "preparer, approver");
    await sees(p, "valuations/bad.csv");
    await say(p, "The fund administrator's valuation for the day");
    await tab(p, "Prices");
    await p.selectOption("#pfund", "growth"); await p.fill("#pdate", "2027-11-12");
    await p.locator("#show-val").click();
    await sees(p, "1010000");
    await say(p, "The maker prepares the unit price from it");
    await p.locator("#prepare button").first().click();
    await sees(p, "Someone else approves it.");
    await tab(p, "Work waiting");
    await p.locator("#work").getByText("growth:2027-11-12.1").waitFor();
    await sees(p, "units on issue");
    // Opening a price from the work list moves to the Prices view by itself.
    await p.getByRole("button", { name: "Details" }).first().click();
    await p.locator("#detail:not([hidden])").waitFor();
    // The preparer cannot approve their own price.
    await say(p, "The maker cannot approve their own price");
    await tab(p, "Work waiting");
    await p.getByRole("button", { name: "Approve", exact: true }).click();
    await p.locator("#work-msg .banner.bad").waitFor();
  }, expectStatus: [409] });
  await visit(browser, "/pricing", { subject: "sub-lead" }, { run: async (p) => {
    await sees(p, "lead");
    await tab(p, "Funds");
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
    await tab(p, "Controls");
    await p.locator("#aml").getByText("No open cases.").waitFor();
    await p.fill("#aml-case", "c1"); await p.fill("#aml-note", "a parent pays");
    await p.locator("#aml-form button").click();
    await p.locator("#aml-msg").getByText(/case/i).waitFor();
  } });
  await visit(browser, "/group", { subject: "sub-grpview" }, { run: async (p) => {
    // A reader sees the queue but is offered nothing to act with.
    await p.locator("#ex").getByText("possible match").waitFor();
    await tab(p, "Members");
    if (await p.locator("#rm-form").isVisible()) throw new Error("a reader was offered a decision");
  } });
  await visit(browser, "/group", { subject: "sub-grp1" }, { run: async (p) => {
    // Staff decide the possible match, and try a mapping without storing it.
    await p.locator("#ex").getByText("possible match").waitFor();
    await say(p, "A payroll row might be an existing person: staff decide the match");
    await tab(p, "Members");
    await p.fill("#rm-scheme", "harbour"); await p.fill("#rm-emp", "E002"); await p.fill("#rm-party", "party-x");
    await p.locator("#rm-form button:not([type=button])").click();
    await p.locator("#rm-msg").getByText("Recorded.").waitFor();
    await tab(p, "Work waiting");
    await p.locator("#ex-form button").click();
    await p.locator("#ex").getByText("Nothing is waiting.").waitFor();
    await say(p, "A new file layout is tried against the scheme's mapping, storing nothing");
    await tab(p, "Files");
    await p.fill("#mp-scheme", "harbour");
    await p.getByRole("button", { name: "Show the mappings in use" }).click();
    await p.locator("#mp").getByText("harbour-freight").first().waitFor();
    await p.fill("#mp-sample", "Emp No\nE001\nE003\n");
    await p.locator("#mp-try").click();
    await p.locator("#mp-msg").getByText("Nothing was stored.").waitFor();
  } });
  await visit(browser, "/finance", { subject: "sub-gllead" }, { expectStatus: [409], run: async (p) => {
    await say(p, "A proposed rate table is priced against the book before anyone approves it");
    await p.getByRole("tab", { name: "Rate tables" }).click();
    const rates = require("path").join(__dirname, "../../../../products/rates/life_base.star");
    await p.fill("#dr-date", "2027-10-31"); await p.fill("#dr-covers", "life");
    await p.setInputFiles("#dr-current", rates); await p.setInputFiles("#dr-proposed", rates);
    await p.locator("#dr button").click();
    await p.locator("#dr-msg").getByText("may go forward to approval").waitFor();
    await sees(p, "Movement by rating cell");
    // The lead sorts policies into accounting groups, and is told plainly why a cut-over total is refused.
    await p.getByRole("tab", { name: "Aged items and set-up" }).click();
    await tab(p, "Books");
    await p.locator("#ag-classify").click();
    await sees(p, "Classified.");
    await say(p, "The tax and levy accounts, reconciled to the posting types that may move them");
    await p.fill("#tb-period", "2027-11"); await p.selectOption("#tb-what", "tax_reconciliation"); await p.locator("#tb button").click();
    await p.locator("#tb-out .banner").first().waitFor();
    await say(p, "A return is filed on a definition kept as reviewed source; an open period is refused");
    await tab(p, "Returns");
    await p.locator("#rd-list").getByText("insurer-quarterly@1").waitFor();
    await p.fill("#rf-id", "q-2027-4"); await p.fill("#rf-period", "2027-11"); await p.locator("#rf-send").click();
    await p.locator("#rf-msg .banner.bad").waitFor();
    await tab(p, "Journals");
    await p.fill("#ye-id", "ye-2027"); await p.fill("#ye-period", "2027-11"); await p.fill("#ye-evidence", "board minute 12"); await p.locator("#ye-send").click();
    await p.locator("#ye-msg .banner.bad").waitFor();
    await tab(p, "Aged items and set-up");
    await p.fill("#co-account", "1999"); await p.fill("#co-currency", "NZD");
    await p.fill("#co-total", "100000"); await p.fill("#co-evidence", "legacy TB");
    await p.locator("#co-send").click();
    await sees(p, "not a migration control account");
  } });
  await visit(browser, "/claims", { subject: "sub-cllead" }, { run: async (p) => {
    await sees(p, "You act as cl-lead");
    await tab(p, "Investigations");
    await p.locator("#inv-show").click();
    await p.locator("#inv").getByText("No cases are open to you.").waitFor();
    await say(p, "She reads the wording a cover is bound to, at the version it was bound to");
    await tab(p, "Wordings");
    await p.fill("#w-filter", "cancer");
    await p.locator("#w-list tbody tr").filter({ has: p.getByRole("cell", { name: "cancer", exact: true }) }).first().getByRole("button", { name: "Read" }).click();
    await p.locator("#w-title").getByText("Cancer, version 1").waitFor();
    await p.locator("#w-text").getByText("wording version 1").waitFor();
    await p.getByRole("button", { name: "Version 2" }).click();
    await p.locator("#w-text").getByText("wording version 2").waitFor();
    await p.fill("#w-filter", "umbrella");
    await p.locator("#w-list").getByRole("button", { name: "Read" }).first().click();
    await p.locator("#w-text").getByText("Page 1 of").first().waitFor();
    await say(p, "A trauma claim is notified");
    await tab(p, "Claim");
    await p.selectOption("#a-command", "notify");
    await p.fill("#f-0", "c-b1"); await p.fill("#f-1", "pol-c1"); await p.fill("#f-2", "trauma");
    await p.selectOption("#f-4", "trauma"); await p.fill("#f-5", "2027-02-20");
    await p.locator("#act button").click();
    await sees(p, "Done: notify.");
    await sees(p, "c-b1: trauma");
    await say(p, "It appears in the lump sum queue and is assigned to an assessor");
    await tab(p, "Queue");
    await p.selectOption("#q-name", "lump_sum"); await p.locator("#queue button").click();
    await p.locator("#q-rows").getByText("c-b1").waitFor();
    await tab(p, "Claim");
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
    // Every decision the claims module takes has its form: suspending a benefit in payment is one.
    await p.selectOption("#a-command", "decide_suspend_benefit");
    await p.locator("#a-fields").getByText("Evidence awaited").waitFor();
    await p.fill("#f-1", "2027-11-20"); await p.fill("#f-2", "medical report"); await p.fill("#f-3", "awaiting_evidence");
    await p.locator("#act button").click();
    await p.locator("#act-msg .banner.bad").waitFor();
    await p.selectOption("#a-command", "close_investigation");
    await p.locator("#a-fields").getByText("Claims to decline").waitFor();
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
    await tab(p, "Cases");
    await p.selectOption("#c-what", "propose");
    await p.fill("#cf-cover", "life"); await p.selectOption("#cf-outcome", "accept");
    await p.fill("#cf-reasons", "RC-0101"); await p.fill("#cf-notes", "vault://notes");
    await p.locator("#c-form button").click();
    await p.locator("#c-msg").getByText("Done.").waitFor();
    await tab(p, "Proposals");
    await p.fill("#p-id", "P2"); await p.locator("#p-form button").first().click();
    await p.locator("#proposal").getByText("P2:").waitFor();
    await say(p, "The applicant's disclosures, read for a stated purpose");
    await p.fill("#d-life", "L1"); await p.fill("#d-purpose", "assess the case"); await p.locator("#d-form button").click();
    await p.locator("#disclosures details").first().waitFor();
    await tab(p, "Conflicts and reports");
    await p.getByRole("button", { name: "Try again" }).click();
    await sees(p, "is back in the outbox");
    await sees(p, "Nothing is parked.");
    await tab(p, "Remedies and review");
    if (await p.locator("#senior").isVisible()) throw new Error("amy sees the senior's tools");
  } });
  await visit(browser, "/underwriting", { subject: "sub-sam" }, { run: async (p) => {
    await sees(p, "a senior underwriter");
    await takeFrom(p, ["senior_review"]);
    await say(p, "Non-disclosure found at claim: he varies the cover, and a second senior must sign");
    await tab(p, "Remedies and review");
    await p.fill("#n-review", "ndr/CLM-1/L1"); await p.selectOption("#n-outcome", "vary");
    await p.fill("#n-reasons", "RC-0980"); await p.fill("#n-cover", "life"); await p.fill("#n-loading", "50");
    await p.locator("#n-form button").click();
    await sees(p, "A second senior signs it");
    await p.getByRole("button", { name: "Withdraw" }).waitFor();
  } });
  await visit(browser, "/underwriting", { subject: "sub-cat" }, { run: async (p) => {
    await tab(p, "Remedies and review");
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
    await tab(p, "Controls");
    await p.fill("#raw-cmd", JSON.stringify({ type: "add_run_items", run: "rb1", items: [{ item: "ib1", source: "browser", counterparty: { payee: "p1" }, amount_minor: 1000, policy: null, payable: null, refund: null, represents: null }] }));
    await p.locator("#raw-form button").click();
    await p.locator("#raw-msg").getByText("Done.").waitFor();
    await tab(p, "Runs");
    await p.locator("#run-form button").click();
    await p.locator("#run").getByText("ib1").waitFor();
    await p.getByRole("button", { name: "Check" }).click();
    await p.locator("#run-msg").getByText("Done: check run.").waitFor();
    if (await p.getByRole("button", { name: "Approve" }).count()) throw new Error("a maker is offered approval");
  } });
  await visit(browser, "/payments", { subject: "sub-payap" }, { run: async (p) => {
    await sees(p, "approver");
    await say(p, "Treasury reconciles the bank account, then approves the run");
    await tab(p, "Reconciliation");
    await p.fill("#rec-account", "anz-sf1-claims"); await p.locator("#rec-form button").click();
    await sees(p, "closing 10000000.00");
    await tab(p, "Runs");
    await p.locator("#runs").getByText("rb1").waitFor();
    await p.locator("#runs").getByRole("button", { name: "Open" }).first().click();
    await p.getByRole("button", { name: "Approve" }).click();
    await p.locator("#run-msg").getByText("Done: approve run.").waitFor();
    await tab(p, "Payees and refunds");
    await p.fill("#party-id", "party-p1"); await p.locator("#party-form button").click();
    await p.locator("#party").getByText("p1").first().waitFor();
  } });
  await visit(browser, "/payments", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/adviser", { subject: "sub-fap" }, { run: async (p) => {
    await sees(p, "Advisers under you");
    await p.locator("#practice").getByText("adv-1").waitFor();
    // The group's own statement, with its overrides totalled.
    await tab(p, "Commission");
    await p.fill("#st-number", "1"); await p.locator("#st-form button").click();
    await p.locator("#statement").getByText("Override (group)").waitFor();
  } });
  await visit(browser, "/replacements", { subject: "sub-wbcon" }, { run: async (p) => {
    await sees(p, "You decide disputed matches");
    await p.locator("#matches").getByText("pol-new/life/1").waitFor();
  } });
  await visit(browser, "/commission", { subject: "sub-dadmin" }, { run: async (p) => {
    await sees(p, "distribution admin");
    await p.getByRole("button", { name: "Show", exact: true }).click();
    await p.locator("#answer dl, #answer table, #answer span").first().waitFor();
    // A question with fields of its own, asked through its form.
    await p.selectOption("#q", "held_control"); await p.fill("#q-date", "2027-11-30");
    await p.getByRole("button", { name: "Show", exact: true }).click();
    await p.locator("#answer dl, #answer table, #answer span").first().waitFor();
    // An adviser is added from a form, with no JSON to write.
    await tab(p, "Change something");
    await p.selectOption("#a", "add_party"); await p.fill("#a-party", "adv-77"); await p.selectOption("#a-kind", "adviser");
    await p.fill("#a-purpose", "onboarding");
    await p.getByRole("button", { name: "Do it" }).click();
    await p.locator("#do-msg").getByText("Done.").waitFor();
    // The few commands with no form still go as written.
    await tab(p, "Another command");
    await p.fill("#cmd", JSON.stringify({ add_party: { party: "adv-78", kind: "adviser" } }));
    await p.fill("#purpose", "onboarding");
    await p.getByRole("button", { name: "Send" }).click();
    await p.locator("#send-msg").getByText("Done.").waitFor();
  } });
  await visit(browser, "/commission", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/replacements", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/reinsurance", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/claims", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/collections", { subject: "sub-colof" }, { run: async (p) => {
    await say(p, "The officer's queue: the policy nearest to lapse first");
    await p.locator("#queue").getByText(/pol-c1|Nothing is in arrears/).first().waitFor();
    await tab(p, "Tasks");
    await p.locator("#t-form button").click();
    await p.locator("#tasks").getByText(/pol-c1|No open tasks|None/).first().waitFor();
  } });
  await visit(browser, "/collections", { subject: "sub-colcfg" }, { run: async (p) => {
    await tab(p, "Sequences");
    await say(p, "Configuration loads the shipped best estimate for a product, checks it and sets it");
    await p.fill("#seq-product", "term-life");
    await p.locator("#seq-load button").click();
    await sees(p, "Nobody has approved it");
    await p.locator("#seq-form button").click();
    await sees(p, "Set for term-life.");
  } });
  await visit(browser, "/collections", { subject: "sub-gl1" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await visit(browser, "/finance", { subject: "sub-sue" }, { expectStatus: [403], run: async (p) => { await p.locator(".banner.bad").first().waitFor(); } });
  await browser.close();
  if (failures.length) { console.error(failures.join("\n")); process.exit(1); }
  console.log("all pages check out");
})();
