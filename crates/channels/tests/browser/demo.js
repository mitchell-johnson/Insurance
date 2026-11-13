// Records the persona demonstrations: one guided video per persona, against
// the seeded portal.
//
//   cargo test -p ic-channels --test http serve_for_a_browser -- --ignored &
//   VIDEO=out POLICY=pol-00000001 node crates/channels/tests/browser/demo.js [http://127.0.0.1:8099]
//
// Each video opens with a card saying who the person is and what they will
// do, captions every step in a bar above the page (never over it), rings the
// thing being used, and ends on the result. Where the system stops someone
// (a maker approving their own work), the bar turns amber and says it is a
// control, so a refusal is never mistaken for a fault. A journey that does
// not end as scripted fails the run rather than being recorded wrong.
// `record.sh` wraps this and writes the MP4s and their index.
const { chromium } = require("playwright");
const fs = require("fs");
const base = process.argv[2] || "http://127.0.0.1:8099";
const out = process.env.VIDEO;
const only = process.env.ONLY;
if (!out) { console.error("set VIDEO to the folder for the recordings"); process.exit(2); }
const failures = [];
const index = [];
const BAR = 64;

const card = (kicker, title, lines) => `<!doctype html><meta charset="utf-8"><body style="margin:0;height:100vh;display:flex;align-items:center;justify-content:center;background:#141820;color:#fff;font-family:system-ui,sans-serif">
<div style="max-width:900px;padding:48px"><div style="font-size:20px;letter-spacing:.08em;text-transform:uppercase;color:#8fb4ff">${kicker}</div>
<h1 style="font-size:46px;line-height:1.15;margin:12px 0 28px">${title}</h1>
<ul style="font-size:24px;line-height:1.6;padding-left:26px;margin:0;color:#dfe5f0">${lines.map((l) => `<li>${l}</li>`).join("")}</ul></div></body>`;

class Demo {
  constructor(page) { this.page = page; this.answers = []; this.allowed = []; this.problems = []; }
  // Put the caption bar above the page and push the page down so nothing is hidden.
  async bar(text, control) {
    await this.page.evaluate(([t, c, height]) => {
      let b = document.getElementById("demo-bar");
      if (!b) {
        b = document.createElement("div");
        b.id = "demo-bar";
        Object.assign(b.style, { position: "fixed", left: "0", right: "0", top: "0", height: height + "px", boxSizing: "border-box", padding: "0 24px", zIndex: 99999,
          display: "flex", alignItems: "center", color: "#fff", font: "600 20px/1.3 system-ui, sans-serif", pointerEvents: "none" });
        document.body.appendChild(b);
        document.body.style.marginTop = height + "px";
        // The rail and a phone's bar stick below the caption, not under it.
        for (const el of document.querySelectorAll(".side, .work > .bar")) { el.style.top = height + "px"; if (el.classList.contains("side")) el.style.height = `calc(100vh - ${height}px)`; }
        document.documentElement.style.scrollPaddingTop = height + 16 + "px";
      }
      b.style.background = c ? "#9a5b00" : "#141820";
      b.textContent = (c ? "Control: " : "") + t;
    }, [text, !!control, BAR]);
  }
  async say(text, hold = 2200) { await this.bar(text, false); await this.page.waitForTimeout(hold); }
  // The system stopping someone, on purpose.
  async control(text, hold = 3200) { await this.bar(text, true); await this.page.waitForTimeout(hold); }
  loc(target) { return typeof target === "string" ? this.page.locator(target).first() : target; }
  async ring(target, hold = 900) {
    const l = this.loc(target);
    await l.waitFor({ timeout: 8000 });
    await l.evaluate((el) => { el.scrollIntoView({ block: "center", behavior: "instant" }); el.dataset.demoOutline = el.style.outline; el.style.outline = "3px solid #f5a623"; el.style.outlineOffset = "3px"; });
    await this.page.waitForTimeout(hold);
    await l.evaluate((el) => { el.style.outline = el.dataset.demoOutline || ""; el.style.outlineOffset = ""; });
    return l;
  }
  async click(target) { const l = await this.ring(target, 700); await l.click(); await this.page.waitForTimeout(500); }
  async type(target, text) { const l = await this.ring(target, 350); await l.fill(""); await l.pressSequentially(String(text), { delay: 28 }); await this.page.waitForTimeout(250); }
  async pick(target, value) { const l = await this.ring(target, 350); await l.selectOption(value); await this.page.waitForTimeout(350); }
  // Wait for a result, bring it into view and let it be read.
  async see(target, hold = 1800) { await this.ring(typeof target === "string" && !/^[#.\[]/.test(target) ? this.page.getByText(target, { exact: false }).first() : target, hold); }
  async intro(kicker, title, lines, hold = 5000) { await this.page.setContent(card(kicker, title, lines)); await this.page.waitForTimeout(hold); }
  // Move to one of a page's views by its tab.
  async tab(name) { await this.click(this.page.getByRole("tab", { name, exact: true })); }
  // Answer a question the page asks in a dialog.
  async answer(text) { await this.type("dialog[open] input", text); await this.click("dialog[open] .actions button:last-child"); }
  // Open a page as someone. Signing in is the development sign-in card, filled here.
  async open(path, subject, roles) {
    if (subject) {
      if (this.page.url().startsWith(base)) await this.page.evaluate(() => window.sessionStorage.clear());
      else { await this.page.goto(base + "/status"); await this.page.evaluate(() => window.sessionStorage.clear()); }
    }
    await this.page.goto(base + path, { waitUntil: "networkidle" });
    await this.bar("");
    if (subject) {
      await this.page.fill("#dev-subject", subject);
      await this.page.fill("#dev-roles", roles || "");
      await this.page.click("#dev-sign-in button");
      await this.page.waitForLoadState("networkidle");
    }
  }
}

async function video(name, persona, title, lines, run) {
  if (only && !name.includes(only)) return;
  const browser = await chromium.launch();
  const context = await browser.newContext({ recordVideo: { dir: `${out}/${name}`, size: { width: 1280, height: 800 } }, viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  const d = new Demo(page);
  page.on("pageerror", (e) => d.problems.push("script error: " + e.message));
  page.on("response", (r) => { if (r.status() >= 400 && !d.allowed.includes(r.status())) d.problems.push(`${r.status()} ${r.url()}`); });
  page.on("dialog", (dialog) => { d.problems.push("browser dialog: " + dialog.message()); dialog.dismiss(); });
  try {
    await d.intro(persona, title, lines);
    await run(d, page);
    await page.waitForTimeout(1500);
    await d.intro("What you saw", title, lines, 3500);
  } catch (e) {
    d.problems.push(String(e.message).split("\n").slice(0, 3).join(" / "));
  }
  await context.close();
  await browser.close();
  fs.writeFileSync(`${out}/${name}/caption.txt`, `${persona}\n${title}\n${lines.join("; ")}\n`);
  for (const p of d.problems) failures.push(`${name}: ${p}`);
  console.log(`${d.problems.length ? "FAIL" : "ok  "} ${name}`);
  index.push(name);
}

(async () => {
  const policy = process.env.POLICY || "pol-00000001";

  await video("01-adviser", "D1 Tessa Maddox, adviser", "An adviser's day in the portal", [
    "See her engagement and the commission held from her", "Dispute a new policy matched as a replacement",
    "Open a client's policy: cover, billing, claims, documents", "Prepare a change for the client to approve in their app"], async (d, p) => {
    d.allowed = [404];
    await d.open("/adviser", "sub-adv");
    await d.say("Tessa signs in. Her engagement with her advice provider is active");
    await d.see("#engagements");
    await d.say("Commission held from her, with the amount and the reason");
    await d.see("#held", 2600);
    await d.say("A new policy she wrote was matched as replacing older cover, which earns no initial commission");
    await d.see(p.locator("#matches"));
    await d.say("It was not a replacement, so she disputes it and says why");
    await d.click(p.getByRole("button", { name: "Dispute" }));
    await d.type("#d-reason", "New cover after a needs review, not a replacement");
    await d.click(p.locator("#dispute button").first());
    await d.see("Disputed. The commission");
    await d.say("She opens a client's policy");
    await d.click(p.getByRole("button", { name: "Open" }).first());
    await p.locator("#detail:not([hidden])").waitFor();
    await d.see(p.locator("#rows"), 2600);
    await d.say("Claims show their status only: what a claim is about stays with the customer and the claims team");
    await d.see(p.locator("#claims"));
    await d.say("The policy's documents are listed");
    await d.see(p.locator("#docs"));
    await d.click(p.locator("#docs").getByRole("button", { name: "Download" }));
    await p.locator("#docs-msg").getByText("not available to you").waitFor();
    await d.control("the schedule is the customer's own document, so the adviser cannot download it");
    await d.say("The client asked by phone to decline this year's indexation. Tessa prepares it");
    await d.type("#note", "Client asked by phone");
    await d.click("#send");
    await d.see("Sent");
    await d.say("Nothing changes until the client approves it in their own app, on their own device", 3000);
  });

  await video("02-advice-provider", "D6 Harbourline Advice, financial advice provider", "An advice provider's view of its practice", [
    "See the advisers engaged with the practice", "Read the provider's own commission statement, with overrides totalled"], async (d, p) => {
    await d.open("/adviser", "sub-fap");
    await d.say("The provider signs in to the same portal and sees its own practice");
    await d.see("Advisers under you");
    await d.see(p.locator("#practice"), 2600);
    await d.say("Health details and claims stay with the adviser and the customer: the provider sees the book only");
    await d.say("Its commission statement, line by line, with totals by what earned it");
    await d.type("#st-number", "1");
    await d.click(p.locator("#st-form button"));
    await p.locator("#statement").getByText("Override (group)").waitFor();
    await d.see(p.locator("#statement"), 3500);
  });

  await video("03-employer", "E1 Kai Fresh Supermarkets, scheme administrator", "An employer runs its staff scheme", [
    "Open the scheme: members, cover and billing", "Upload this month's payroll file", "Preview who joins, leaves and changes, then approve it",
    "Look up a member, without ever seeing health details"], async (d, p) => {
    await d.open("/employer", "sub-hr");
    await d.say("The scheme administrator signs in and sees the employer's schemes");
    await d.click(p.getByRole("button", { name: "Open" }).first());
    await p.locator("#detail:not([hidden])").waitFor();
    await d.say("Members, total cover and what is owed");
    await d.see("#summary");
    await d.see("#members");
    await d.see("#billing", 2600);
    await d.say("Claims show a status only. What a claim is about never reaches the employer");
    await d.see("#claims");
    await p.locator("#group:not([hidden])").waitFor();
    await d.say("This month's payroll file is uploaded. It is checked for viruses before anything reads it");
    await d.ring("#file");
    await p.setInputFiles("#file", { name: "nov 2027.csv", mimeType: "text/csv", buffer: Buffer.from("employee_id,salary\nE001,80000\n") });
    await d.click(p.getByRole("button", { name: "Upload" }));
    await d.see(p.locator("#file-msg").getByText("Uploaded"));
    await d.say("The preview shows who joins, leaves and changes before anything is applied");
    await d.click(p.getByRole("button", { name: "Show preview" }));
    await p.locator("#file-msg").getByText("quarantined").waitFor();
    await d.see("#file-view", 2600);
    await d.say("The administrator approves the file");
    await d.click("#approve-file");
    await d.see(p.locator("#file-msg").getByText("approved"));
    await d.say("One member's record: category, salary and cover");
    await d.type("#employee", "E001");
    await d.click(p.getByRole("button", { name: "Show", exact: true }));
    await p.locator("#member-view").getByText("staff").waitFor();
    await d.see("#member-view", 3000);
    await d.say("Health loadings, underwriting and claim details are never shown to an employer", 3000);
  });

  await video("04-junior-underwriter", "ST1 Junior underwriter", "An underwriter works a case", [
    "Take the next case from the queue", "Propose standard acceptance within her authority", "Read the applicant's disclosures for a stated purpose"], async (d, p) => {
    await d.open("/underwriting", "sub-amy");
    await d.say("Amy signs in. The console tells her who she is and what she holds");
    await d.see("You are amy");
    await d.say("She takes the next case waiting in her queues");
    let taken = false;
    for (const q of ["medical", "financial", "occupational", "tele_interview"]) {
      await p.selectOption("#q-name", q);
      await Promise.all([p.waitForResponse((r) => r.url().includes("/underwriting/queue?")), p.locator("#q-form button").click()]);
      await p.waitForTimeout(200);
      const take = p.locator("#queue").getByRole("button", { name: "Take" });
      if (await take.count()) { await d.see("#queue"); await d.click(take.first()); await d.see("You hold"); taken = true; break; }
    }
    if (!taken) throw new Error("no case was waiting");
    await d.say("She proposes standard acceptance of the life cover, with a reason code and her notes");
    await d.pick("#c-what", "propose");
    await d.type("#cf-cover", "life"); await d.pick("#cf-outcome", "accept");
    await d.type("#cf-reasons", "RC-0101"); await d.type("#cf-notes", "vault://notes");
    await d.click(p.locator("#c-form button"));
    await d.see(p.locator("#c-msg").getByText("Done."));
    await d.say("The proposal she is working on");
    await d.type("#p-id", "P2"); await d.click(p.locator("#p-form button").first());
    await p.locator("#proposal").getByText("P2:").waitFor();
    await d.see("#proposal", 2600);
    await d.say("She reads the applicant's health disclosures. The purpose is recorded with the read");
    await d.type("#d-life", "L1"); await d.type("#d-purpose", "assess the case"); await d.click(p.locator("#d-form button"));
    await p.locator("#disclosures details").first().waitFor();
    await d.see("#disclosures", 2600);
    await d.say("A senior underwriter's tools are not offered to her", 2600);
  });

  await video("05-senior-underwriters", "ST2 Senior underwriters", "Two seniors decide a non-disclosure remedy", [
    "The first senior takes the review and varies the cover", "The system holds it for a second senior", "The second senior signs, and only then is it sent"], async (d, p) => {
    await d.open("/underwriting", "sub-sam");
    await d.say("Sam is a senior underwriter. A claim found something not disclosed at application");
    await d.see("a senior underwriter");
    await p.selectOption("#q-name", "senior_review");
    await Promise.all([p.waitForResponse((r) => r.url().includes("/underwriting/queue?")), p.locator("#q-form button").click()]);
    await d.see("#queue");
    await d.click(p.locator("#queue").getByRole("button", { name: "Take" }).first());
    await d.see("You hold");
    await d.say("He varies the cover: a 50% loading instead of avoiding the policy");
    await d.type("#n-review", "ndr/CLM-1/L1"); await d.pick("#n-outcome", "vary");
    await d.type("#n-reasons", "RC-0980"); await d.type("#n-cover", "life"); await d.type("#n-loading", "50");
    await d.click(p.locator("#n-form button"));
    await d.see("A second senior signs it");
    await d.control("a remedy that changes someone's cover is never one person's decision. It waits for a second senior");
    await d.intro("Next", "The second senior underwriter", ["Cat signs in separately and sees what is waiting for her"], 3000);
    await d.open("/underwriting", "sub-cat");
    await d.say("Cat sees the remedy Sam prepared, waiting for a second senior");
    await d.see(p.getByRole("button", { name: "Sign and send" }));
    await d.click(p.getByRole("button", { name: "Sign and send" }));
    await d.see("Signed and sent.");
    await d.say("Signed by two seniors, the remedy is sent. Nothing is left waiting", 2600);
  });

  await video("06-claims-assessor", "ST4 Senior claims assessor", "A claim is notified and assigned", [
    "Notify a trauma claim", "Find it in the lump sum queue", "Assign it to an assessor, with the reason recorded"], async (d, p) => {
    d.allowed = [403];
    await d.open("/claims", "sub-cllead");
    await d.say("The claims lead signs in. Every action is taken in her own name");
    await d.see("You act as cl-lead");
    await d.say("A trauma claim is notified against a policy");
    await d.pick("#a-command", "notify");
    await d.type("#f-0", "c-b1"); await d.type("#f-1", "pol-c1"); await d.type("#f-2", "trauma");
    await d.pick("#f-4", "trauma"); await d.type("#f-5", "2027-02-20");
    await d.click(p.locator("#act button"));
    await d.see("Done: notify.");
    await d.see("c-b1: trauma");
    await d.say("It is waiting in the lump sum queue, oldest and most urgent first");
    await d.pick("#q-name", "lump_sum"); await d.click(p.locator("#queue button"));
    await d.see(p.locator("#q-rows").getByText("c-b1"));
    await d.say("She assigns it to an assessor, saying why she is acting");
    await d.type("#a-purpose", "allocating lump sum work");
    await d.pick("#a-command", "assign"); await d.type("#f-1", "cl-1");
    await d.click(p.locator("#act button"));
    await d.see("Done: assign.");
    await d.see(p.locator("#c-summary"), 2600);
    await d.say("She then tries to record the claim's cause herself");
    await d.pick("#a-command", "record_cause"); await d.type("#f-1", "CC-0003");
    await d.click(p.locator("#act button"));
    await p.locator("#act-msg .banner.bad").waitFor();
    await d.ring("#act-msg");
    await d.control("only the assessor who owns a claim records its cause. Even the lead is refused");
  });

  await video("07-service-officer", "ST5 Service officer, then a second approver", "A change for a customer who rang", [
    "Open the policy for the case being worked, with the access logged", "Enter the change the customer asked for",
    "It is above her limit, so a colleague must agree", "Then it waits for the customer to approve in their app"], async (d, p) => {
    await d.open("/staff", "sub-sue");
    await d.say("Sue takes a call. She opens the policy by naming the case she is working on");
    await d.type("#pol", policy); await d.type("#case", "case-1");
    await d.click(p.locator("#lookup button"));
    await p.locator("#pol-view:not([hidden])").waitFor();
    await d.say("The policy, as far as her role allows. Anything above her data classes is withheld and counted");
    await d.see("#pol-rows", 3000);
    await d.see("#pol-advisers");
    await d.say("The customer wants a change. Sue enters it, with the reason and the amount");
    await d.type("#creason", "Customer rang"); await d.type("#camount", "900");
    await d.click(p.locator("#for-customer button"));
    const said = await p.locator("#cust-msg").getByText("recorded").textContent();
    const entered = said.match(/Request (\S+) recorded/)[1];
    await d.see("#cust-msg", 2600);
    await d.control("$900 is above Sue's limit, so the change goes to a second member of staff before the customer");
    await d.intro("Next", "The second approver", ["Alice signs in separately", "She cannot be the person who entered the change"], 3000);
    await d.open("/staff", "sub-alice");
    await d.say("Alice agrees her colleague's change");
    await d.type("#second-id", entered);
    await d.click(p.locator("#second button"));
    await d.see(p.locator("#second-msg").getByText("Agreed."), 2600);
    await d.say("It now waits for the customer's own approval in their app. Staff cannot approve for them", 3000);
  });

  await video("08-group-administrator", "ST7 Group administrator", "Staff look after an employer's scheme", [
    "Work the exceptions queue", "Decide whether a payroll row is an existing person", "Try a new file layout without storing anything"], async (d, p) => {
    await d.open("/group", "sub-grp1");
    await d.say("The exceptions queue: everything about schemes that needs a person");
    await d.see(p.locator("#ex").getByText("possible match"), 2600);
    await d.say("A payroll row might be someone the insurer already knows. She confirms the match");
    await d.type("#rm-scheme", "harbour"); await d.type("#rm-emp", "E002"); await d.type("#rm-party", "party-x");
    await d.click(p.locator("#rm-form button"));
    await d.see(p.locator("#rm-msg").getByText("Recorded."));
    await d.say("The queue is now clear");
    await d.click(p.locator("#ex-form button"));
    await d.see(p.locator("#ex").getByText("Nothing is waiting."));
    await d.say("The employer's file layout changed this year. She looks at the mapping in use");
    await d.type("#mp-scheme", "harbour");
    await d.click(p.getByRole("button", { name: "Show the mappings in use" }));
    await p.locator("#mp").getByText("harbour-freight").first().waitFor();
    await d.see("#mp", 2600);
    await d.say("And tries it against a sample of the new file. A trial stores nothing");
    await d.type("#mp-sample", "Emp No\nE001\nE003\n");
    await d.click("#mp-try");
    await d.see(p.locator("#mp-msg").getByText("Nothing was stored."));
    await d.see("#mp", 3000);
  });

  await video("09-unit-pricing", "ST8 Unit pricing maker, then the pricing lead", "Striking a unit price", [
    "Read the fund administrator's valuation", "Prepare the day's price", "The maker cannot approve their own price",
    "The lead suspends a fund during an outage"], async (d, p) => {
    d.allowed = [409];
    await d.open("/pricing", "sub-up1");
    await d.say("The maker signs in. Valuation files the administrator sent are listed, including one that failed its checks");
    await d.see("valuations/bad.csv");
    await d.say("He opens the growth fund's valuation for the day");
    await d.pick("#pfund", "growth"); await d.type("#pdate", "2027-11-12");
    await d.click("#show-val");
    await d.see("#valuation", 2600);
    await d.say("And prepares the unit price from it");
    await d.click(p.locator("#prepare button").first());
    await d.see("Someone else approves it.");
    await d.see(p.locator("#work").getByText("growth:2027-11-12.1"));
    await d.click(p.getByRole("button", { name: "Details" }).first());
    await p.locator("#detail:not([hidden])").waitFor();
    await d.see("#detail", 3000);
    await d.say("He tries to approve it himself");
    await d.click(p.getByRole("button", { name: "Approve", exact: true }));
    await p.locator("#work-msg .banner.bad").waitFor();
    await d.ring("#work-msg");
    await d.control("a price is prepared by one person and approved by another. The maker is refused");
    await d.intro("Next", "The unit pricing lead", ["The fund administrator has an outage", "The lead suspends pricing for the cash fund"], 3000);
    d.allowed = [];
    await d.open("/pricing", "sub-lead");
    await d.say("The lead suspends the cash fund from a date, with the reason");
    await d.pick("#sfund", "cash"); await d.type("#sfrom", "2027-11-15"); await d.type("#sreason", "administrator outage");
    await d.click(p.locator("#suspend button"));
    await d.see("Suspended.");
    await d.see("administrator outage (lead-1)", 2600);
  });

  await video("10-finance-close", "ST10 Finance operator, then the approver", "Month end in the ledger", [
    "See what is stopping the close", "Report a check done", "Propose a journal with its reason and evidence",
    "A second person approves and posts it", "The trial balance shows it"], async (d, p) => {
    await d.open("/finance", "sub-gl1");
    await d.say("The finance operator opens November's close");
    await d.type("#p-id", "2027-11"); await d.click(p.locator("#period button"));
    await p.locator("#dash:not([hidden])").waitFor();
    await d.see("Stopping the close", 2600);
    await d.see("#dash", 2600);
    await d.say("He reports the tax reconciliation as done");
    await d.pick("#r-check", "tax_reconciliation"); await d.click(p.locator("#report button"));
    await d.see("Reported.");
    await d.say("A month-end accrual needs a manual journal");
    await d.click(p.getByRole("tab", { name: "Journals" }));
    await d.type("#j-id", "bj1"); await d.type("#j-period", "2027-11"); await p.fill("#j-date", "2027-11-30");
    await d.type("#j-reason", "Month-end accrual"); await d.type("#j-evidence", "vault://evidence/1");
    await d.type("#j-lines", "6500, 250.00, SF1-life\n2500, -250.00, SF1-life");
    await d.click(p.locator("#journal button"));
    await d.see("Someone else approves it.");
    await p.locator("#jview:not([hidden])").waitFor();
    await d.see("#jview", 2600);
    if (!(await p.locator("#j-approve").isDisabled())) throw new Error("an operator was offered approval");
    await d.ring("#j-approve");
    await d.control("the operator proposed this journal, so the Approve button is not his to press");
    await d.intro("Next", "The finance approver", ["A different person signs in", "They read the journal, then approve and post it"], 3000);
    await d.open("/finance", "sub-gl2");
    await d.click(p.getByRole("tab", { name: "Journals" }));
    await d.say("The approver looks up the journal and reads it");
    await d.type("#jl-id", "bj1"); await d.click(p.locator("#jlookup button"));
    await p.locator("#jview:not([hidden])").waitFor();
    await d.see("#jview", 2600);
    await d.click("#j-approve");
    await d.see("Approved and posted.");
    await d.say("The trial balance for the month now carries both sides");
    await d.click(p.getByRole("tab", { name: "Books" }));
    await d.type("#tb-period", "2027-11"); await d.click(p.locator("#tb button"));
    await p.locator("#tb-out").getByText("6500").first().waitFor();
    await d.see("#tb-out", 3200);
  });

  await video("11-held-commission", "ST10 Finance operator, then a second finance officer", "Releasing commission held from an adviser", [
    "See what is held and why", "Prepare a release with its evidence", "The preparer cannot approve it", "A second officer approves"], async (d, p) => {
    d.allowed = [403, 409, 422];
    await d.open("/staff", "sub-fin1");
    await d.say("Commission is held from an adviser whose licence is suspended");
    await d.click(p.getByRole("tab", { name: "Held balances" }));
    await d.see("adv-1");
    await d.click(p.getByRole("button", { name: "Open" }).first());
    await p.locator("#position:not([hidden])").waitFor();
    await d.see("suspended licence", 2600);
    await d.say("She prepares a release of $100, with the account, reason code and supporting document");
    await d.type("#amt", "100.00"); await d.type("#ref", "12-3456-7890123-00"); await d.type("#reason", "REL-01"); await d.type("#doc", "doc-1");
    await d.click(p.locator("#prepare button"));
    await d.see("Prepared.");
    await d.see("awaiting approval");
    await d.say("She tries to approve it herself");
    await d.click(p.getByRole("button", { name: "Approve", exact: true }));
    await p.locator("#bench-msg .banner.bad").waitFor();
    await d.ring("#bench-msg");
    await d.control("the person who prepared an action is never the one who approves it");
    await d.say("Her own access is hers to read: roles, limits and every record she has opened");
    await d.click(p.getByRole("tab", { name: "My access" }));
    await d.see("finance", 2200);
    await d.say("She declares a personal connection. That person's policies are then closed to her");
    await d.type("#conn", "party-00000009"); await d.click(p.locator("#connect button"));
    await d.see("Declared.");
    await d.see(p.locator("#connections").getByText("party-00000009"), 2600);
  });

  await video("12-payments", "ST10 Payment run maker, then ST17 Treasury approver", "A payment run, from preparation to approval", [
    "Prepare a provider payment run", "Check it", "Treasury reconciles the bank account", "Treasury approves the run"], async (d, p) => {
    await d.open("/payments", "sub-paymk");
    await d.say("The maker signs in. The console says which payment roles she holds");
    await d.see("You are pay-mk: maker.");
    await d.say("She prepares a provider payment run for a value date");
    await d.type("#r-id", "rb1"); await d.pick("#r-type", "provider_payments"); await p.fill("#r-date", "2027-11-16");
    await d.click(p.locator("#prep button"));
    await d.see("Prepared. Add items");
    await p.fill("#raw-cmd", JSON.stringify({ type: "add_run_items", run: "rb1", items: [{ item: "ib1", source: "browser", counterparty: { payee: "p1" }, amount_minor: 1000, policy: null, payable: null, refund: null, represents: null }] }));
    await p.locator("#raw-form button").click();
    await p.locator("#raw-msg").getByText("Done.").waitFor();
    await d.say("A payment to a provider is added to the run");
    await d.click(p.locator("#run-form button"));
    await d.see(p.locator("#run").getByText("ib1"), 2400);
    await d.say("She checks the run: totals, payees and holds");
    await d.click(p.getByRole("button", { name: "Check" }));
    await d.see(p.locator("#run-msg").getByText("Done: check run."));
    if (await p.getByRole("button", { name: "Approve" }).count()) throw new Error("a maker is offered approval");
    await d.control("a maker is not offered approval. The run waits for treasury");
    await d.intro("Next", "The treasury approver", ["A different person signs in", "They reconcile the bank account and approve the run"], 3000);
    await d.open("/payments", "sub-payap");
    await d.say("Treasury first reconciles the claims bank account");
    await d.type("#rec-account", "anz-sf1-claims"); await d.click(p.locator("#rec-form button"));
    await d.see("closing 10000000.00", 2400);
    await d.say("The run the maker checked is waiting");
    await d.see(p.locator("#runs").getByText("rb1"));
    await d.click(p.locator("#runs").getByRole("button", { name: "Open" }).first());
    await d.see("#run", 2400);
    await d.click(p.getByRole("button", { name: "Approve" }));
    await d.see(p.locator("#run-msg").getByText("Done: approve run."), 2600);
    await d.say("A payee is shown without account details. Those stay with the payee maintainer");
    await d.type("#party-id", "party-p1"); await d.click(p.locator("#party-form button"));
    await d.see(p.locator("#party"), 3000);
  });

  await video("13-complaints", "Frontline staff, the complaints handler and the conduct officer", "A complaint, from first hearing to the register", [
    "Anyone who hears a complaint records it", "The handler takes it from the queue and opens its file",
    "It appears in the month's register", "The conduct officer opens a conduct case"], async (d, p) => {
    await d.open("/complaints", "sub-front");
    await d.say("A frontline member of staff hears a complaint on a call and records it there and then");
    await d.see("intake");
    await d.type("#i-id", "c-100"); await d.type("#i-who", "party-00000001"); await d.type("#i-doc", "doc-100");
    await d.click(p.locator("#intake button"));
    await d.see("Recorded c-100", 2600);
    await d.intro("Next", "The complaints handler", ["Trained to work with vulnerable customers", "Takes the complaint, opens its file, checks the register"], 3000);
    await d.open("/complaints", "sub-cmp1");
    await d.see("trained to work with vulnerable customers");
    await d.say("The queue: vulnerable complainants first, then the nearest deadline");
    await d.click(p.getByRole("tab", { name: "Queue" }));
    await d.see(p.locator("#queue").getByText("c-100"));
    await d.say("She assigns the complaint to herself");
    await d.pick("#a-command", "assign");
    await d.type("#f-0", "c-100"); await d.type("#f-1", "cmp-1");
    await d.click(p.locator("#act button"));
    await d.see("Done: assign.");
    await d.say("The complaint's file, with its running deadlines");
    await d.click(p.locator("#queue").getByRole("button", { name: "Open" }).first());
    await p.locator("#file:not([hidden])").waitFor();
    await d.see("#file", 3200);
    await d.say("The register and report for the month");
    await d.click(p.getByRole("tab", { name: "Register and report" }));
    await p.fill("#p-from", "2027-11-01"); await p.fill("#p-to", "2027-11-30");
    await d.click(p.locator("#period button"));
    await d.see(p.locator("#register").getByText("c-100"), 2600);
    await d.intro("Next", "The conduct officer", ["Conduct cases are open to the conduct function and legal only", "The officer opens a case for a suspected breach"], 3000);
    await d.open("/complaints", "sub-con");
    await d.click(p.getByRole("tab", { name: "Queue" }));
    await d.say("The conduct officer opens a conduct case and owns it");
    await d.pick("#a-command", "open_conduct_case");
    await d.type("#f-0", "b-100"); await d.type("#f-2", "con-1");
    await d.click(p.locator("#act button"));
    await d.see("Done: open conduct case.");
    await d.click(p.getByRole("tab", { name: "Conduct cases" }));
    await d.click(p.locator("#cases").getByRole("button", { name: "Open" }).first());
    await p.locator("#case:not([hidden])").waitFor();
    await d.see("Owner con-1");
    await d.see("#case", 3000);
  });

  await video("14-conduct-replacements", "Conduct officer", "Deciding a disputed replacement", [
    "See new cover matched as replacing old cover", "The adviser's dispute waits for the conduct officer"], async (d, p) => {
    await d.open("/replacements", "sub-wbcon");
    await d.say("New cover written soon after cover on the same life was given up earns no initial commission");
    await d.see("You decide disputed matches");
    await d.say("The match the adviser disputed, with both policies and the amount matched");
    await d.see("#matches", 3500);
  });

  await video("15-reinsurance", "Reinsurance operations, then the reinsurance lead", "Abandoning a failed reinsurance instruction", [
    "See the exception and why it failed", "Propose abandoning the instruction", "A second person signs before it is sent"], async (d, p) => {
    await d.open("/reinsurance", "sub-ri1");
    await d.say("An instruction to the core failed and is listed as an exception, with the reason");
    await d.see("you act on exceptions");
    await d.see(p.locator("#exceptions"), 3000);
    await d.say("The policy was entered in error, so the instruction should be abandoned");
    await d.click(p.getByRole("button", { name: "Abandon the instruction" }));
    await d.see("stay apart");
    await d.type("#a-reason", "The policy was entered in error");
    await d.click(p.locator("#act-form button").first());
    await d.see("Someone else signs it");
    await d.control("abandoning an instruction takes two people. It waits for a second signature");
    await d.intro("Next", "The reinsurance lead", ["A different person signs in", "They read what was prepared, then sign and send it"], 3000);
    await d.open("/reinsurance", "sub-ri2");
    await d.say("The lead sees the abandonment waiting, with who prepared it and why");
    await d.see(p.getByRole("button", { name: "Sign and send" }));
    await d.click(p.getByRole("button", { name: "Sign and send" }));
    await d.see("Signed and sent.");
    await d.see("No exceptions are open.", 2600);
  });

  await video("16-finance-lead", "ST11 Finance lead", "Sorting policies into accounting groups", [
    "Classify every policy not yet in a group", "A wrong cut-over account is refused in plain words"], async (d, p) => {
    d.allowed = [409];
    await d.open("/finance", "sub-gllead");
    await d.click(p.getByRole("tab", { name: "Aged items and set-up" }));
    await d.say("Each policy falls in an accounting group for reporting. The lead classifies those not yet placed");
    await d.click("#ag-classify");
    await d.see("Classified.", 2400);
    await d.say("At migration, the old system's total is recorded against a control account");
    await d.type("#co-account", "1999"); await d.type("#co-currency", "NZD");
    await d.type("#co-total", "100000"); await d.type("#co-evidence", "legacy TB");
    await d.click("#co-send");
    await d.see("not a migration control account");
    await d.control("account 1999 is not a migration control account, so the ledger refuses the total and says why");
  });

  await video("17-operations", "ST16 Platform engineer", "Watching the platform", [
    "The public status page anyone can read", "Region health and fail-over readiness, for operations only"], async (d, p) => {
    await d.open("/status");
    await d.say("The public status page: is the service up, and when it was last checked");
    await d.see("Last checked", 2600);
    await d.open("/ops", "sub-ops", "operations");
    await d.say("The operations console: each region's health and whether the cluster holds quorum");
    await d.see("auckland", 2400);
    await d.see("sydney", 2400);
    await d.say("A region that is down is shown plainly, with what that means for fail-over", 3000);
  });

  fs.writeFileSync(`${out}/videos.json`, JSON.stringify(index));
  if (failures.length) { console.error(failures.join("\n")); process.exit(1); }
  console.log("all demonstrations recorded");
})();
