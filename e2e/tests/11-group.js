// Group schemes: the group console (group administration staff) and the
// employer portal (an employer's scheme administrators), over the group
// service, with the customer master behind it deciding who each joiner is.
//
// The stack runs the group service and its driver, the customer master on
// PostgreSQL, and the Azure stand-in for the quarantine store. Defender for
// Storage is not there to scan an uploaded file, so a test writes the scan
// result on the blob as Defender would (`scan`). Everything after that is the
// system's own: the driver reads the file, the master matches the joiners,
// the core takes the members and bills them.
//
// A scheme a test changes is registered for that test (`own`). The standing
// scheme `kaifresh` is the seed's, and tests only read it.
"use strict";
const crypto = require("crypto");
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");
const oracle = require("../lib/oracle");
const group = require("../lib/group");

const A = "Group schemes";
const G = "/staff/v1/group/";
const FOUNDER = ["E001", "Ana", "Founder", "01/05/1980", "staff", 60000, "akl"];

/// A scheme of this test's own, with its founding member.
const own = (t) => group.register(t.sys, t.ref("sch").toLowerCase());
/// A person nobody else in the customer master resembles: the master matches on name and date of birth, so a
/// name shared with another test's member would be put to staff as a possible match.
function person(employee, category = "staff", salary = 60000, site = "wlg") {
  const letters = crypto.randomBytes(8).toString("hex").replace(/\d/g, (d) => "ghijklmnop"[d]);
  const day = 1 + crypto.randomInt(28), month = 1 + crypto.randomInt(12), year = 1965 + crypto.randomInt(30);
  return [employee, `G${letters.slice(0, 7)}`, `F${letters.slice(7)}`, `${String(day).padStart(2, "0")}/${String(month).padStart(2, "0")}/${year}`, category, salary, site];
}
const employerSays = (t, scheme, command, who = cast.e1) => t.sys.portal("POST", `/employer/v1/schemes/${scheme.policy}/group/commands`, who, command);
const standing = async (t, scheme) => (await t.does(cast.st7, "GET", `${G}scheme?scheme=${scheme.id}`, undefined, "reads the scheme")) ;
/// The employer's page, with one of their schemes opened.
async function opened(t, scheme, who = cast.e1) {
  const s = await t.open("/employer", who);
  await s.row("#schemes", scheme.number).getByRole("button", { name: "Open" }).click();
  await s.sees(`Scheme ${scheme.number}`, "#title");
  return s;
}
/// The group console, on one scheme.
async function console_(t, scheme, who = cast.st7) {
  const s = await t.open("/group", who);
  await s.tab("Schemes");
  await s.fill("#sc-id", scheme.id);
  await s.press("Show", "#sc-form");
  await s.sees(scheme.policy, "#sc");
  return s;
}
const choose = (s, name, rows) => s.page.setInputFiles("#file", { name, mimeType: "text/csv", buffer: Buffer.from(group.csv(rows)) });

// --- The group console. -----------------------------------------------------------------------------------------

test({ id: "E2E-301", area: A, persona: "ST7", story: "US-ST07-1", title: "A group administrator arrives able to act, with the work waiting on a scheme in front of them", matrix: [],
  purpose: "Sign in to the group console as a group administrator and show the work waiting on the standing scheme.",
  why: "The console must say what this person may do, and the exception queue is where their day starts; an empty queue must say it is empty." }, async (t) => {
  const s = await t.open("/group", cast.st7);
  await s.sees("You are st7: group administration.", "#me");
  await s.fill("#ex-scheme", "kaifresh");
  await s.press("Show", "#ex-form");
  await s.sees("Nothing is waiting.", "#ex");
});

test({ id: "E2E-302", area: A, persona: "ST7", story: "US-ST07-1", title: "A read-only group administrator sees a scheme and is offered nothing that changes it", matrix: [],
  purpose: "Sign in with the view-only role, show the standing scheme, and try to freeze it by the route the button uses.",
  why: "Reading schemes is given more widely than acting on them; the page must not offer, and the system must not accept, a change from a reader." }, async (t) => {
  const s = await console_(t, { id: "kaifresh", policy: "pol-kaifresh" }, cast.st7r);
  await s.sees("You are st7r: view only.", "#me");
  await s.never("Freeze");
  await s.never("End a scheme");
  const r = await t.sys.portal("POST", G + "commands", cast.st7r, { freeze: { scheme: "kaifresh", reason: "a reader trying" } });
  if (r.status !== 403 || !String(r.data.detail).includes("you may read group schemes but not act on them")) throw new Error(`a reader's freeze answered ${r.status} ${JSON.stringify(r.data)}`);
  if ((await standing(t, { id: "kaifresh" })).frozen) throw new Error("the scheme was frozen by a reader");
});

test({ id: "E2E-303", area: A, persona: "ST5", story: "US-ST07-1", title: "Staff outside group administration are refused the group console", matrix: [],
  purpose: "Open the group console as a customer service officer.",
  why: "Scheme files hold salaries and membership of whole workforces; other staff have no reason to read them." }, async (t) => {
  const s = await t.open("/group", cast.st5);
  await s.refused("your roles do not include group schemes");
});

test({ id: "E2E-304", area: A, persona: "ST7", story: "US-ST07-1", title: "A scheme is found from its policy, with its standing and its terms", matrix: [],
  purpose: "Give the console the scheme's policy instead of its name and read what comes back.",
  why: "An employer on the phone quotes the policy; the administrator must reach the scheme from it, and see whether it is active, frozen or blocked." }, async (t) => {
  const s = await t.open("/group", cast.st7);
  await s.tab("Schemes");
  await s.fill("#sc-policy", "pol-kaifresh");
  await s.press("Show", "#sc-form");
  await s.sees("pol-kaifresh", "#sc");
  if ((await s.$("#sc-id").inputValue()) !== "kaifresh") throw new Error("the console did not name the scheme on the policy");
  const rows = Object.fromEntries(await s.rows("#sc"));
  if (!/active/.test(rows.status || "")) throw new Error(`the scheme's status reads "${rows.status}"`);
  await s.page.locator("#sc details summary").click();
  await s.sees("backdating limit days", "#sc");
  // A policy with no scheme on it says so.
  await s.fill("#sc-id", ""); await s.fill("#sc-policy", "pol-nothing-here");
  await s.press("Show", "#sc-form");
  await s.stopped("#sc-msg", "No scheme is written on that policy.");
});

test({ id: "E2E-305", area: A, persona: "ST7", story: "US-ST07-1", title: "Freezing a scheme needs a reason, stops its files, and is undone by unfreezing", matrix: [],
  purpose: "Freeze a scheme without a reason, then with one; send a file as the employer; unfreeze and send it again.",
  why: "A freeze is how staff stop a scheme's files during a dispute or an audit. It must be explained, must actually stop files, and must come off cleanly." }, async (t) => {
  const scheme = await own(t);
  const s = await console_(t, scheme);
  await s.press("Freeze", "#sc-act");
  await s.stopped("#sc-msg", "Say why the scheme is frozen.");
  await s.fill("#sc-reason", "membership audit");
  await s.press("Freeze", "#sc-act");
  await s.done("#sc-msg", "Done.");
  await s.sees("membership audit", "#sc");
  const held = await group.upload(t.sys, scheme, "nov.csv", [FOUNDER, person("E002")]);
  if (held.status !== 422 || held.data.error.code !== "scheme_frozen") throw new Error(`a file sent to a frozen scheme answered ${held.status} ${JSON.stringify(held.data)}`);
  await s.press("Unfreeze", "#sc-act");
  await s.done("#sc-msg", "Done.");
  await s.never("membership audit", "#sc");
  const taken = await group.upload(t.sys, scheme, "nov.csv", [FOUNDER, person("E002")]);
  if (taken.status !== 202) throw new Error(`a file sent after the freeze came off answered ${taken.status} ${JSON.stringify(taken.data)}`);
});

test({ id: "E2E-306", area: A, persona: "ST7", story: "US-B24-2", title: "A member's cover is their category's multiple of salary, in place up to the automatic limit", matrix: ["GRP-009"],
  purpose: "Apply a file with a staff member and an executive on 240,000, then show each member and compare cover with the independent calculator.",
  why: "Cover by formula is what the employer bought. The part above the automatic acceptance limit must wait for underwriting, and the member must be covered to the limit meanwhile." }, async (t) => {
  const scheme = await own(t);
  await group.applied(t.sys, scheme, "nov.csv", [FOUNDER, person("E002", "staff", 85000), person("E003", "executive", 240000)]);
  const s = await console_(t, scheme);
  await s.tab("Members");
  for (const [employee, category, salary] of [["E002", "staff", 85000], ["E003", "executive", 240000]]) {
    const want = oracle.groupMember(category, salary * 100);
    await s.fill("#mb-emp", employee);
    await s.press("Show", "#mb-form");
    await s.sees(employee, "#mb");
    const tables = s.$("#mb").locator("table");
    const cover = (await tables.nth(1).locator("tbody tr").first().locator("td").allInnerTexts()).map((c) => c.trim());
    if (cover[2] !== oracle.money(want.formula) || cover[3] !== oracle.money(want.automatic)) throw new Error(`${employee}: the screen shows cover ${cover[2]} with ${cover[3]} automatic; the calculator gives ${oracle.money(want.formula)} and ${oracle.money(want.automatic)}`);
    if (want.above) {
      const above = (await tables.nth(2).locator("tbody tr").first().locator("td").allInnerTexts()).map((c) => c.trim());
      if (above[1] !== oracle.money(want.above) || above[2] !== "pending" || above[3] !== "0.00") throw new Error(`${employee}: cover above the limit reads ${JSON.stringify(above)}; the calculator gives ${oracle.money(want.above)} asked, pending, nothing in place`);
    } else if (await tables.count() > 2) throw new Error(`${employee} is shown cover above the automatic limit and has none`);
  }
});

test({ id: "E2E-307", area: A, persona: "ST7", story: "US-ST07-1", title: "Asking for someone who is not a member says so", matrix: [],
  purpose: "Show an employee ID the scheme does not have.",
  why: "A wrong ID must not look like a member with no cover; the administrator must be told the person is not on the scheme." }, async (t) => {
  const s = await console_(t, { id: "kaifresh", policy: "pol-kaifresh" });
  await s.tab("Members");
  await s.fill("#mb-emp", "E999");
  await s.press("Show", "#mb-form");
  await s.stopped("#mb-msg", "E999 is not a member of scheme kaifresh", 404);
});

test({ id: "E2E-308", area: A, persona: "ST7", story: "US-E04-1", title: "A file layout is tried against a sample and nothing is stored", matrix: ["GRP-020"],
  purpose: "Show the layout in use, try it against a sample with one unreadable date, and check the scheme is as it was.",
  why: "A layout is proven on the employer's sample before it is used. The trial must show what would join and what would be refused, and must change nothing." }, async (t) => {
  const scheme = await own(t);
  const s = await console_(t, scheme);
  await s.tab("Files");
  await s.press("Show the mappings in use", "#mp-form");
  await s.sees("kai-fresh", "#mp");
  const joiner = person("E002");
  await s.fill("#mp-sample", group.csv([FOUNDER, joiner, ["E009", "Di", "Unreadable", "31/02/1990", "staff", 50000, "akl"]]));
  await s.press("Try it");
  await s.done("#mp-msg", "Tried. Nothing was stored.");
  const text = (await s.$("#mp").innerText()).toLowerCase().replace(/\s+/g, " ");
  for (const words of ["joiners", "rejected", "e009", "bad date", "date of birth"]) if (!text.includes(words)) throw new Error(`the trial does not show "${words}": ${text.slice(0, 900)}`);
  const after = await standing(t, scheme);
  if (after.members !== 1 || after.open_file !== null) throw new Error(`the trial changed the scheme: ${after.members} members, open file ${after.open_file}`);
});

test({ id: "E2E-309", area: A, persona: "ST7", story: "US-E04-1", title: "A file whose columns have moved is refused by the old layout, naming the missing column", matrix: ["GRP-020"],
  purpose: "Try the layout in use against a sample from a payroll export that renamed its columns.",
  why: "Payroll exports change. The old layout must refuse the new file outright and say which column it could not find, not read salaries from the wrong column." }, async (t) => {
  const s = await console_(t, { id: "kaifresh", policy: "pol-kaifresh" });
  await s.tab("Files");
  await s.press("Show the mappings in use", "#mp-form");
  await s.sees("kai-fresh", "#mp");
  await s.fill("#mp-sample", "Employee,First,Last,Born,Category,Salary,Site\nE001,Ana,Founder,1980-05-01,staff,60000,akl\n");
  await s.press("Try it");
  await s.stopped("#mp-msg", "the file has no column headed \"Emp No\"", 409);
});

test({ id: "E2E-310", area: A, persona: "ST7", story: "US-ST07-1", title: "Staff cannot send the commands that belong to the file driver", matrix: [],
  purpose: "Use Another command to record a scan result for a file, as only the driver may.",
  why: "A clean scan result is what lets a file out of quarantine. A person typing one in would defeat the virus check." }, async (t) => {
  const s = await console_(t, { id: "kaifresh", policy: "pol-kaifresh" });
  await s.fill("#raw-cmd", JSON.stringify({ record_scan: { scheme: "kaifresh", file: "nov.csv", clean: true } }));
  await s.press("Send", "#raw-form");
  await s.stopped("#raw-msg", "record_scan is not a command staff send from the console", 400);
});

test({ id: "E2E-311", area: A, persona: "ST7", story: "US-E01-1", title: "Ending a scheme removes its members on the date and leaves each a continuation offer", matrix: ["GRP-034"],
  purpose: "End a scheme of three members from the console and show its standing afterwards.",
  why: "When an employer's scheme ends, every member loses cover on one day and each is owed the chance to continue it personally. None may be left on cover or without an offer." }, async (t) => {
  const scheme = await own(t);
  await group.applied(t.sys, scheme, "nov.csv", [FOUNDER, person("E002"), person("E003")]);
  const s = await console_(t, scheme);
  await s.fill("#tm-date", t.stack.opening);
  await s.fill("#tm-reason", "the employer closed the scheme");
  await s.page.check("#tm-sure");
  await s.press("End the scheme", "#tm-form");
  await s.done("#tm-msg", `The scheme ends on ${t.stack.opening}.`);
  let after;
  for (let n = 0; n < 40; n++) { after = await standing(t, scheme); if (after.status.status === "terminated" && after.members === 0 && after.open_offers === 3) break; await s.wait(250); }
  if (after.status.status !== "terminated" || after.members !== 0 || after.open_offers !== 3) throw new Error(`after ending: status ${JSON.stringify(after.status)}, ${after.members} members, ${after.open_offers} continuation offers (expected terminated, 0 and 3)`);
  await s.press("Show", "#sc-form");
  await s.sees("terminated", "#sc");
});

// --- The employer portal. ---------------------------------------------------------------------------------------

test({ id: "E2E-312", area: A, persona: "E1", story: "US-E01-1", title: "An employer's administrator sees their scheme and opens it", matrix: [],
  purpose: "Sign in to the employer portal as the scheme's administrator and open the scheme.",
  why: "The administrator's first question is what the insurer holds for their scheme: who is on it, and what is owed." }, async (t) => {
  const s = await opened(t, { number: "WPL-KAIFRESH" });
  await s.sees("total cover", "#summary");
  await s.tab("Files and members");
  await s.sees("on the group service's record", "#standing");
});

test({ id: "E2E-313", area: A, persona: "E3", story: "US-E03-1", title: "Another employer sees none of this employer's scheme", matrix: [],
  purpose: "Sign in as an administrator of a different employer, and ask for the first employer's scheme by its policy.",
  why: "Two employers on one portal must never see each other's workforce. A scheme that is not yours must look as if it does not exist." }, async (t) => {
  const s = await t.open("/employer", cast.e3);
  await s.never("WPL-KAIFRESH");
  for (const path of ["scheme", "invoices", "member?employee=E001"]) {
    const r = await t.sys.portal("GET", `/employer/v1/schemes/pol-kaifresh/group/${path}`, cast.e3);
    if (r.status !== 404) throw new Error(`another employer asking for ${path} was answered ${r.status} ${JSON.stringify(r.data).slice(0, 200)}`);
  }
  const sent = await group.upload(t.sys, { policy: "pol-kaifresh" }, "theirs.csv", [FOUNDER], cast.e3);
  if (sent.status !== 404) throw new Error(`another employer's file was answered ${sent.status}`);
});

test({ id: "E2E-314", area: A, persona: "E1", story: "US-E01-1", title: "A membership file goes from upload through the virus check and preview to members on cover, billed to the cent", matrix: ["GRP-001", "GRP-012"],
  purpose: "Upload a file with two joiners from the employer's page, preview it once scanned, approve it, and compare what the scheme then owes with the independent calculator.",
  why: "This is the employer's monthly job and the scheme's whole premium. The file must wait for its scan, show who joins before anything changes, and bill exactly the sum of the members' premiums." }, async (t) => {
  const scheme = await own(t);
  const joiners = [person("E002", "staff", 72500, "wlg"), person("E003", "executive", 240000, "wlg")];
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await choose(s, "nov.csv", [FOUNDER, ...joiners]);
  await s.press("Upload", "#upload");
  await s.done("#file-msg", "Uploaded. It is being checked");
  await s.press("Show preview", "#preview");
  await s.done("#file-msg", "The file is quarantined.");
  if ((await standing(t, scheme)).members !== 1) throw new Error("members changed before the file was scanned");
  await group.scan(t.sys, scheme, "nov.csv");
  const preview = await group.reaches(t.sys, scheme, "nov.csv", "awaiting_approval");
  if (preview.summary.joiners !== 2 || preview.summary.above_automatic_cover !== 1) throw new Error(`the preview counts ${JSON.stringify(preview.summary)}`);
  await s.press("Show preview", "#preview");
  await s.done("#file-msg", "The file is awaiting approval.");
  await s.sees("joiners", "#file-view");
  if ((await standing(t, scheme)).members !== 1) throw new Error("members changed before the file was approved");
  await s.press("Approve", "#preview");
  await s.done("#file-msg", "The file is");
  await group.reaches(t.sys, scheme, "nov.csv", "applied");
  await s.press("Show preview", "#preview");
  await s.done("#file-msg", "The file is applied.");
  // The first month's bill: every member's premium, founder included.
  const owed = [["staff", 60000], ...joiners.map((j) => [j[4], j[5]])].reduce((sum, [category, salary]) => sum + oracle.groupMember(category, salary * 100).monthly, 0);
  let after;
  for (let n = 0; n < 60; n++) { after = await standing(t, scheme); if (after.members === 3 && after.outstanding_minor === owed) break; await s.wait(250); }
  if (after.members !== 3) throw new Error(`the scheme has ${after.members} members after the file, expected 3`);
  if (after.outstanding_minor !== owed) throw new Error(`the scheme owes ${oracle.money(after.outstanding_minor)}; the calculator gives ${oracle.money(owed)}`);
  const c = await console_(t, scheme);
  const rows = Object.fromEntries(await c.rows("#sc"));
  if (rows["outstanding minor"] !== oracle.money(owed)) throw new Error(`the console shows ${rows["outstanding minor"]} outstanding; the calculator gives ${oracle.money(owed)}`);
});

test({ id: "E2E-315", area: A, persona: "E1", story: "US-E01-1", title: "A file the virus scan condemns is refused and never read", matrix: ["GRP-001"],
  purpose: "Upload a file, have the scan report it malicious, and try to approve it.",
  why: "Files come from outside. One that fails its scan must never be parsed or applied, and the employer must be told it was refused." }, async (t) => {
  const scheme = await own(t);
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await choose(s, "nov.csv", [FOUNDER, person("E002")]);
  await s.press("Upload", "#upload");
  await s.done("#file-msg", "Uploaded.");
  await group.scan(t.sys, scheme, "nov.csv", false);
  const refused = await group.reaches(t.sys, scheme, "nov.csv", "refused");
  if (refused.summary.rows !== 0) throw new Error(`a condemned file was read: ${JSON.stringify(refused.summary)}`);
  await s.press("Show preview", "#preview");
  await s.done("#file-msg", "The file is refused.");
  await s.press("Approve", "#preview");
  await s.stopped("#file-msg", "", 422);
  if ((await standing(t, scheme)).members !== 1) throw new Error("a condemned file changed the membership");
});

test({ id: "E2E-316", area: A, persona: "E1", story: "US-E01-1", title: "A file withdrawn at its preview changes nobody's cover", matrix: [],
  purpose: "Upload a file that would add two members, read its preview, and withdraw it.",
  why: "The preview exists so a wrong file can be taken back. Withdrawing must leave the scheme exactly as it was and free it for the right file." }, async (t) => {
  const scheme = await own(t);
  await group.upload(t.sys, scheme, "wrong.csv", [FOUNDER, person("E002"), person("E003")]);
  await group.scan(t.sys, scheme, "wrong.csv");
  await group.reaches(t.sys, scheme, "wrong.csv", "awaiting_approval");
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await s.fill("#fname", "wrong.csv");
  await s.press("Withdraw", "#preview");
  await s.done("#file-msg", "The file is withdrawn.");
  const after = await standing(t, scheme);
  if (after.members !== 1 || after.open_file !== null) throw new Error(`after withdrawing: ${after.members} members, open file ${after.open_file}`);
  const next = await group.upload(t.sys, scheme, "right.csv", [FOUNDER, person("E002")]);
  if (next.status !== 202) throw new Error(`the next file was answered ${next.status} ${JSON.stringify(next.data)}`);
});

test({ id: "E2E-317", area: A, persona: "E1", story: "US-BK1-1", title: "The same file sent twice changes nothing the second time", matrix: ["GRP-003"],
  purpose: "Apply a file, then send the same content again under a new name and under its own name.",
  why: "Employers resend files when unsure they arrived. A second copy must not add anyone twice or bill twice." }, async (t) => {
  const scheme = await own(t);
  const rows = [FOUNDER, person("E002"), person("E003")];
  await group.applied(t.sys, scheme, "nov.csv", rows);
  const before = await standing(t, scheme);
  const renamed = await group.upload(t.sys, scheme, "nov-again.csv", rows);
  if (renamed.status !== 422 || renamed.data.error.code !== "duplicate_file") throw new Error(`the same content under a new name answered ${renamed.status} ${JSON.stringify(renamed.data)}`);
  await group.upload(t.sys, scheme, "nov.csv", rows);
  await new Promise((done) => setTimeout(done, 2500));
  const file = (await group.file(t.sys, scheme, "nov.csv")).data, after = await standing(t, scheme);
  if (file.status !== "applied" || file.applied !== 2) throw new Error(`after the second copy the file reads ${JSON.stringify(file).slice(0, 300)}`);
  if (after.members !== before.members || after.open_file !== null) throw new Error(`the second copy changed the scheme: ${after.members} members, open file ${after.open_file}`);
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await s.sees("3 members on the group service's record", "#standing");
});

test({ id: "E2E-318", area: A, persona: "E1", story: "US-B25-1", title: "Rows that cannot be read are listed with their reasons and the rest of the file goes ahead", matrix: ["GRP-002"],
  purpose: "Send a file in which one row has an impossible date and one a category the scheme does not have, and read the preview on the employer's page and in the console.",
  why: "One bad row must not hold up a whole workforce, and must not be dropped silently: the employer has to know who was left out and why." }, async (t) => {
  const scheme = await own(t);
  const rows = [FOUNDER, person("E002"), ["E004", "Di", "Unreadable", "31/02/1990", "staff", 50000, "akl"], ["E005", "Ed", "Miscategorised", "01/01/1990", "pilot", 50000, "akl"]];
  await group.upload(t.sys, scheme, "nov.csv", rows);
  await group.scan(t.sys, scheme, "nov.csv");
  const preview = await group.reaches(t.sys, scheme, "nov.csv", "awaiting_approval");
  if (preview.summary.joiners !== 1 || preview.summary.rejected !== 2) throw new Error(`the preview counts ${JSON.stringify(preview.summary)}`);
  const c = await console_(t, scheme);
  await c.tab("Files");
  await c.fill("#fi-id", "nov.csv");
  await c.press("Show", "#fi-form");
  await c.sees("Rows refused when read", "#fi");
  const refusedRows = (await c.rows("#fi")).filter(([row]) => /^\d+$/.test(row));
  const want = [["3", "bad date"], ["4", "unknown value"]];
  if (JSON.stringify(refusedRows) !== JSON.stringify(want)) throw new Error(`the console lists refused rows ${JSON.stringify(refusedRows)}, expected ${JSON.stringify(want)}`);
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await s.fill("#fname", "nov.csv");
  await s.press("Show preview", "#preview");
  await s.done("#file-msg", "The file is awaiting approval.");
  await s.sees("E004", "#file-view");
  await s.sees("E005", "#file-view");
  await s.press("Approve", "#preview");
  await s.done("#file-msg", "The file is");
  const done = await group.reaches(t.sys, scheme, "nov.csv", "applied");
  if (done.applied !== 1) throw new Error(`${done.applied} rows were applied, expected the one good joiner`);
  for (const [employee, status] of [["E002", 200], ["E004", 404], ["E005", 404]]) {
    const m = await t.sys.portal("GET", `${G}member?scheme=${scheme.id}&employee=${employee}`, cast.st7);
    if (m.status !== status) throw new Error(`${employee} answers ${m.status} after the file, expected ${status}`);
  }
});

test({ id: "E2E-319", area: A, persona: "ST7", story: "US-B24-1", title: "A file that would remove most of a scheme is held until group administration confirms it", matrix: ["GRP-004"],
  purpose: "With three members on a scheme, send a file listing only one. Try to approve it as the employer, then confirm it from the console.",
  why: "A truncated export looks like mass resignations. Removing most of a workforce's cover must take a second look by the insurer, and the employer alone must not be able to push it through." }, async (t) => {
  const scheme = await own(t);
  await group.applied(t.sys, scheme, "nov.csv", [FOUNDER, person("E002"), person("E003")]);
  await group.upload(t.sys, scheme, "dec.csv", [FOUNDER]);
  await group.scan(t.sys, scheme, "dec.csv");
  const held = await group.reaches(t.sys, scheme, "dec.csv", "held");
  if (held.summary.leavers !== 2) throw new Error(`the held file counts ${JSON.stringify(held.summary)}`);
  const pushed = await employerSays(t, scheme, { approve_file: { file: "dec.csv" } });
  if (pushed.status !== 422 || pushed.data.error.code !== "wrong_file_status") throw new Error(`the employer approving a held file answered ${pushed.status} ${JSON.stringify(pushed.data)}`);
  const s = await t.open("/group", cast.st7);
  await s.fill("#ex-scheme", scheme.id);
  await s.press("Show", "#ex-form");
  await s.row("#ex", "dec.csv").waitFor({ timeout: 10000 });
  const waiting = await s.row("#ex", "dec.csv").locator("td").allInnerTexts();
  if (!waiting.includes("file held")) throw new Error(`the work waiting reads ${JSON.stringify(waiting)}`);
  await s.tab("Files");
  await s.fill("#fi-scheme", scheme.id); await s.fill("#fi-id", "dec.csv");
  await s.press("Show", "#fi-form");
  await s.sees("dec.csv: held", "#fi");
  await s.press("Confirm the held file");
  await s.done("#fi-msg", "Confirmed.");
  await s.sees("dec.csv: awaiting approval", "#fi");
  if ((await standing(t, scheme)).members !== 3) throw new Error("confirming the held file removed members before it was approved");
});

test({ id: "E2E-320", area: A, persona: "E1", story: "US-B02-2", title: "An employer sees a member's category, salary and cover, and nothing about their health or who they are to the insurer", matrix: ["GRP-032"],
  purpose: "Show a member on the employer's page, then compare the fields the employer's route returns with those group administration sees.",
  why: "An employer pays for the scheme but has no right to a member's health, underwriting or identity record. The employer's view must be the narrower one, field by field." }, async (t) => {
  const scheme = await own(t);
  await group.applied(t.sys, scheme, "nov.csv", [FOUNDER, person("E003", "executive", 240000)]);
  const want = oracle.groupMember("executive", 240000 * 100);
  const s = await opened(t, scheme);
  await s.tab("Files and members");
  await s.fill("#employee", "E003");
  await s.press("Show", "#lookup");
  await s.sees("E003: executive", "#member-view");
  await s.sees("on cover", "#member-view");
  const digits = (await s.$("#member-view").innerText()).replace(/[^0-9.]/g, "");
  for (const amount of [want.formula, want.automatic, want.above]) if (!digits.includes(oracle.money(amount))) throw new Error(`the member view does not show ${oracle.money(amount)}: ${(await s.$("#member-view").innerText()).slice(0, 300)}`);
  const theirs = (await t.does(cast.e1, "GET", `/employer/v1/schemes/${scheme.policy}/group/member?employee=E003`, undefined, "reads a member")), ours = (await t.does(cast.st7, "GET", `${G}member?scheme=${scheme.id}&employee=E003`, undefined, "reads a member"));
  for (const hidden of ["health", "occupation_class", "party", "loadings", "candidates"]) if (hidden in theirs) throw new Error(`the employer is given the member's ${hidden}`);
  const extra = Object.keys(theirs).filter((k) => !(k in ours));
  if (extra.length || !("health" in ours)) throw new Error(`the employer's view is not the narrower one: extra ${JSON.stringify(extra)}`);
});

test({ id: "E2E-321", area: A, persona: "E1", story: "US-B25-2", title: "An administrator gives a colleague access, and the colleague then sees the scheme", matrix: ["GRP-028"],
  purpose: "As the scheme's administrator make a colleague an administrator from the page; sign in as the colleague before and after.",
  why: "Administrators change. The employer must be able to hand over without calling the insurer, and the new person must see nothing until they are named." }, async (t) => {
  const scheme = await own(t);
  const before = await t.does(cast.e1b, "GET", "/employer/v1/schemes", undefined, "lists schemes");
  if (before.some((p) => p.policy === scheme.policy)) throw new Error("the colleague sees the scheme before being named");
  const s = await opened(t, scheme);
  await s.tab("Changes and administrators");
  await s.fill("#who", cast.e1b.party);
  await s.press("Save", "#admin");
  await s.done("#admin-msg", "Saved.");
  const b = await opened(t, scheme, cast.e1b);
  await b.tab("Files and members");
  await b.sees("on the group service's record", "#standing");
});

test({ id: "E2E-322", area: A, persona: "E1", story: "US-B25-2", title: "A scheme's only administrator cannot remove themselves", matrix: ["GRP-028"],
  purpose: "As the only administrator of a scheme, remove your own access.",
  why: "A scheme with no administrator cannot send files or approve them; the last one out must hand over first." }, async (t) => {
  const scheme = await own(t);
  const s = await opened(t, scheme);
  await s.tab("Changes and administrators");
  await s.fill("#who", cast.e1.party);
  await s.pick("#is", "false");
  await s.press("Save", "#admin");
  await s.stopped("#admin-msg", "a scheme keeps at least one administrator", 422);
  const mine = await t.does(cast.e1, "GET", "/employer/v1/schemes", undefined, "lists schemes");
  if (!mine.some((p) => p.policy === scheme.policy)) throw new Error("the only administrator was removed");
});

// --- The customer master, behind the group service. -------------------------------------------------------------

test({ id: "E2E-323", area: A, persona: "ST7", story: "US-B10-1", title: "A joiner who may be someone the insurer already knows is put to group administration, with the people they might be", matrix: ["GRP-031"],
  purpose: "Put the same name and date of birth on two schemes. Read the work waiting on the second, and the member's record, as a group administrator.",
  why: "Name and birth date alone do not prove two records are one person. The customer master must not merge them on its own; a person decides, and to decide they must be shown who the candidates are." }, async (t) => {
  const twin = person("E002");
  const first = await own(t), second = await own(t);
  await group.applied(t.sys, first, "nov.csv", [FOUNDER, twin]);
  if ((await standing(t, first)).possible_matches !== 0) throw new Error("the first of the two was already a possible match of someone");
  await group.applied(t.sys, second, "nov.csv", [FOUNDER, ["E777", ...twin.slice(1)]]);
  const s = await t.open("/group", cast.st7);
  await s.fill("#ex-scheme", second.id);
  await s.press("Show", "#ex-form");
  await s.row("#ex", "E777").waitFor({ timeout: 10000 });
  const waiting = await s.row("#ex", "E777").locator("td").allInnerTexts();
  if (!waiting.includes("possible match")) throw new Error(`the work waiting reads ${JSON.stringify(waiting)}`);
  // The member joined, unlinked, and is on cover meanwhile.
  await s.tab("Members");
  await s.fill("#mb-scheme", second.id); await s.fill("#mb-emp", "E777");
  await s.press("Show", "#mb-form");
  await s.sees("E777", "#mb");
  const member = await t.does(cast.st7, "GET", `${G}member?scheme=${second.id}&employee=E777`, undefined, "reads the member");
  if (!member.in_force) throw new Error("the possible match was left without cover while it waits");
  // To decide, the administrator must be told who the member might be.
  const candidates = member.possible_matches || member.candidates;
  if (!Array.isArray(candidates) || !candidates.length) throw new Error("the console gives group administration no way to learn who the member might be: neither the work waiting nor the member's record names the candidate people, and the form to decide the match asks for one");
  await s.sees(candidates[0], "#mb");
});
