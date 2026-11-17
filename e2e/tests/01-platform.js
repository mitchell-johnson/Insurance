// Platform and access: the front page, the public status page, the
// operations console, the frame every console shares, and who is let in.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Platform and access";
const CONSOLES = [
  ["staff", "Staff console"], ["records", "Customer records"], ["claims", "Claims console"], ["underwriting", "Underwriting"], ["group", "Group schemes"], ["complaints", "Complaints and conduct"],
  ["documents", "Documents"], ["payments", "Payments"], ["collections", "Collections"], ["finance", "Finance console"], ["pricing", "Unit pricing console"], ["commission", "Commission console"], ["replacements", "Replacement matches"],
  ["reinsurance", "Reinsurance exceptions"], ["ops", "Operations console"],
];

test({ id: "E2E-001", area: A, persona: "Anyone", story: "US-ST16-1", title: "The front page offers every portal, grouped by who works there",
  purpose: "Open the front page with no sign-in and read the launcher: the customer's page, partner portals, customer consoles, money consoles and platform pages.",
  why: "The front page is how every member of staff and every partner finds their console; a missing card hides a whole area of the system." }, async (t) => {
  const s = await t.open("/", null);
  for (const group of ["Partners", "Customers", "Money", "Platform"]) await s.sees(group, "#launch");
  for (const [, name] of [...CONSOLES, ["customer", "Your policies"], ["adviser", "Adviser portal"], ["employer", "Employer portal"], ["status", "Service status"]]) await s.sees(name, "#launch");
  const links = await s.page.locator("#launch a").count();
  if (links !== CONSOLES.length + 4) throw new Error(`the launcher has ${links} cards, expected ${CONSOLES.length + 4}`);
});

test({ id: "E2E-002", area: A, persona: "Anyone", story: "US-ST16-1", title: "A launcher card takes a person to the page it names",
  purpose: "Press the Service status card on the front page and arrive on the status page.",
  why: "A card that leads nowhere, or to the wrong console, sends staff to the wrong work on their first click." }, async (t) => {
  const s = await t.open("/", null);
  await s.page.locator("#launch a", { hasText: "Service status" }).click();
  await s.page.waitForURL("**/status");
  await s.sees("Whether everything is working, in plain words.");
});

test({ id: "E2E-003", area: A, persona: "P1", story: "US-ST16-2", title: "The public status page says in plain words that the service is working", matrix: [],
  purpose: "Open the status page as a member of the public while every region is healthy.",
  why: "Customers and advisers read this page during an incident; it must answer without a sign-in and state the real health of the running cluster." }, async (t) => {
  const s = await t.open("/status", null);
  await s.sees("All services are working normally", "#status");
  await s.sees("Last checked", "#checked");
  if (await s.page.locator("#dev-sign-in").count()) throw new Error("the public status page asked for a sign-in");
  const kind = await s.page.locator("#status .banner").getAttribute("class");
  if (!kind.includes("ok")) throw new Error(`the status banner is "${kind}", expected the healthy one`);
});

test({ id: "E2E-004", area: A, persona: "ST16", story: "US-ST16-1", title: "The platform engineer sees every region, its nodes and its leaders", matrix: [],
  purpose: "Sign in to the operations console with the operations role and read the regions table of the running three-node cluster.",
  why: "During a region loss this is the screen the incident commander steers by; it must show the real leaders and reachability, not a canned picture." }, async (t) => {
  const s = await t.open("/ops", cast.st16);
  await s.sees("Reachable", "#summary");
  await s.sees(`${t.stack.partitions} of ${t.stack.partitions}`, "#summary");
  await s.sees("At home", "#summary");
  const rows = await s.rows("#regions");
  const auckland = rows.find((r) => r[0] === "auckland"), sydney = rows.find((r) => r[0] === "sydney");
  if (!auckland || !sydney) throw new Error(`regions shown: ${JSON.stringify(rows)}`);
  if (auckland[1] !== "yes" || auckland[2] !== "2 of 2" || auckland[3] !== String(t.stack.partitions)) throw new Error(`auckland reads ${JSON.stringify(auckland)}`);
  if (sydney[1] !== "" || sydney[2] !== "1 of 1" || sydney[3] !== "0") throw new Error(`sydney reads ${JSON.stringify(sydney)}`);
});

test({ id: "E2E-005", area: A, persona: "ST16", story: "US-ST16-2", title: "The operations console shows what customers are being told",
  purpose: "Read the public message on the operations console and compare it with the public status page.",
  why: "The incident commander must see exactly the words customers see; two different messages during an incident is a conduct failure." }, async (t) => {
  const ops = await t.open("/ops", cast.st16);
  await ops.sees("All services are working normally", "#public");
  const pub = await t.open("/status", null);
  const a = await ops.text("#public strong"), b = await pub.text("#status strong");
  if (a !== b) throw new Error(`operations says "${a}", the public page says "${b}"`);
});

test({ id: "E2E-006", area: A, persona: "ST16", story: "US-ST16-1", title: "The operations console shows the channel module's business date and state",
  purpose: "Read the channel module row on the operations console: business date, policies, feeds, pending requests and state hash.",
  why: "A channel module that has stopped hearing the core shows here first; the row must come from the live module." }, async (t) => {
  const s = await t.open("/ops", cast.st16);
  await s.sees("State hash", "#channels");
  const [row] = await s.rows("#channels");
  // The core's business date moves when a test runs the day; it never goes behind the opening date.
  if (!/^\d{4}-\d{2}-\d{2}$/.test(row[0]) || row[0] < t.stack.opening) throw new Error(`business date reads "${row[0]}"`);
  if (row[4] === "never") throw new Error("the channel module has never confirmed the core's stream");
  if (!/^[0-9a-f]{16}$/.test(row[5])) throw new Error(`state hash reads "${row[5]}"`);
});

test({ id: "E2E-007", area: A, persona: "ST3", story: "US-ST16-1", title: "A claims assessor cannot open the operations console", matrix: [],
  purpose: "Sign in to the operations console as a member of staff who does not hold the operations role.",
  why: "Region and leader detail is for the platform team; the refusal must be whole-page and say which role is needed." }, async (t) => {
  const s = await t.open("/ops", cast.st3, 403);
  await s.refused("the operations console needs the operations role");
  await s.never("Partitions with a leader");
});

test({ id: "E2E-008", area: A, persona: "Anyone", story: "US-ST16-1", title: "An unknown sign-in is refused at the operations console",
  purpose: "Sign in to the operations console with a subject the insurer has never registered.",
  why: "The first line of defence for the platform's own console is that a valid-looking sign-in with no role sees nothing." }, async (t) => {
  const s = await t.open("/ops", cast.stranger, 403);
  await s.refused("operations role");
});

// A person the insurer does not know is refused by every console, with only the refusal on screen.
const HIDDEN = { staff: "Look up a policy", claims: "Work queues", underwriting: "Queues", group: "Exception queue", complaints: "Register", payments: "Runs", finance: "Close", pricing: "Valuations", commission: "Advisers", replacements: "Disputed", reinsurance: "Cessions" };
// The first eleven consoles keep the IDs they were given (E2E-009 to 019); consoles added later are numbered from E2E-340.
const LATER = ["records", "documents", "collections"];
let n = 9, later = 340;
for (const [page, name] of CONSOLES.filter(([p]) => p !== "ops")) {
  test({ id: `E2E-${String(LATER.includes(page) ? later++ : n++).padStart(3, "0")}`, area: A, persona: "Anyone", story: "US-ST15-1", title: `An unknown sign-in is refused at ${name}`, matrix: ["SEC-001"],
    purpose: `Sign in to ${name} with a subject that is bound to no staff record and no partner.`,
    why: "Every console holds customer or financial records; a sign-in the insurer never registered must see a refusal and nothing behind it." }, async (t) => {
    const s = await t.open("/" + page, cast.stranger, 403);
    await s.refused();
    const behind = await s.page.locator("main > :not(.gate):not(header)").evaluateAll((els) => els.filter((el) => el.offsetParent !== null).length);
    if (behind) throw new Error(`${behind} part(s) of the page still show behind the refusal`);
    await s.press("Sign in as someone else", ".gate.refused");
    await s.page.locator("#dev-sign-in").waitFor({ state: "visible", timeout: 10000 });
  });
}

test({ id: "E2E-020", area: A, persona: "ST5", story: "US-ST05-1", title: "The side navigation lists every console and marks the one in use",
  purpose: "Open the staff console and read the side navigation: three groups, a link to every console and the status page, the current page marked.",
  why: "Staff move between consoles all day; the navigation is the one map of the system and must be the same on every page." }, async (t) => {
  const s = await t.open("/staff", cast.st5);
  for (const group of ["Customers", "Money", "Platform"]) await s.sees(group, "#side nav");
  const links = await s.page.locator("#side nav a").allInnerTexts();
  if (links.length !== CONSOLES.length + 1) throw new Error(`the navigation has ${links.length} links: ${links.join(", ")}`);
  for (const [, name] of CONSOLES) if (!links.some((l) => l.trim() === name)) throw new Error(`the navigation has no ${name}: ${links.join(", ")}`);
  const current = await s.page.locator('#side nav a[aria-current="page"]').allInnerTexts();
  if (current.length !== 1 || current[0].trim() !== "Staff console") throw new Error(`marked as current: ${JSON.stringify(current)}`);
});

test({ id: "E2E-021", area: A, persona: "ST5", story: "US-ST05-1", title: "The frame shows who is signed in and signs them out",
  purpose: "Sign in as the collections officer, read the name in the frame, sign out and land back on the sign-in card.",
  why: "On a shared desk the person must be able to see whose access is in use and end it; a sign-out that leaves the session behind exposes customer records." }, async (t) => {
  const s = await t.open("/staff", cast.st5);
  await s.sees("sub-st5", ".who");
  await s.sees("Staff", ".who");
  await s.click("#out");
  await s.page.locator("#dev-sign-in").waitFor({ state: "visible", timeout: 10000 });
  const kept = await s.page.evaluate(() => window.sessionStorage.getItem("dev:workforce"));
  if (kept) throw new Error("the sign-in was still stored after signing out");
});

test({ id: "E2E-022", area: A, persona: "ST5", story: "US-ST05-1", title: "The dark theme is chosen once and kept across pages",
  purpose: "Switch to the dark theme on the staff console, reload, and open another console in the same browser.",
  why: "Staff who work nights choose the dark theme; a theme that resets on each page is the kind of friction that makes a system disliked." }, async (t) => {
  const s = await t.open("/staff", cast.st5);
  await s.page.getByRole("button", { name: "Use the dark theme" }).click();
  const theme = () => s.page.evaluate(() => document.documentElement.dataset.theme);
  if ((await theme()) !== "dark") throw new Error("the page did not turn dark");
  await s.page.reload({ waitUntil: "networkidle" });
  if ((await theme()) !== "dark") throw new Error("the theme was lost on reload");
  await s.page.goto(t.stack.portal + "/status", { waitUntil: "networkidle" });
  if ((await theme()) !== "dark") throw new Error("the theme was lost on another page");
  await s.page.getByRole("button", { name: "Use the light theme" }).waitFor({ state: "visible" });
});

test({ id: "E2E-023", area: A, persona: "ST6", story: "US-ST06-1", title: "A person whose device asks for dark gets the dark theme without choosing",
  purpose: "Open the status page in a browser set to prefer dark and check the page's colours follow it.",
  why: "Respecting the device setting is an accessibility need for light-sensitive staff and customers, not a preference." }, async (t) => {
  t.options = { ...t.options, theme: "dark" };
  const s = await t.open("/status", null);
  const bg = await s.page.evaluate(() => getComputedStyle(document.body).backgroundColor);
  const [r, g, b] = bg.match(/\d+/g).map(Number);
  if (r + g + b > 200) throw new Error(`the page background is ${bg} under a dark preference`);
  await s.page.getByRole("button", { name: "Use the light theme" }).waitFor({ state: "visible" });
});

test({ id: "E2E-024", area: A, persona: "ST4", story: "US-ST04-1", title: "On a phone the navigation opens from the menu button and closes again",
  purpose: "Open the claims console at phone width, open the menu, read the consoles, and close it with the scrim.",
  why: "Case managers visit claimants and work from a phone; a navigation that cannot be reached at that width strands them on one page." }, async (t) => {
  t.options = { ...t.options, width: 390 };
  const s = await t.open("/claims", cast.st4);
  const side = s.page.locator("#side");
  const onScreen = async () => (await side.boundingBox()).x >= 0;
  if (await onScreen()) throw new Error("the navigation covers the page before the menu is opened");
  await s.page.getByRole("button", { name: "Menu", exact: true }).click();
  await s.page.waitForTimeout(400);
  if (!(await onScreen())) throw new Error("the menu button did not bring the navigation on screen");
  await s.sees("Finance console", "#side");
  await s.page.locator(".scrim").click({ position: { x: 370, y: 400 } });
  await s.page.waitForTimeout(400);
  if (await onScreen()) throw new Error("the navigation stayed open after the scrim was pressed");
  const wide = await s.page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth + 1);
  if (wide) throw new Error("the page scrolls sideways at phone width");
});

test({ id: "E2E-025", area: A, persona: "ST6", story: "US-ST06-1", title: "A keyboard user moves between a console's views with the arrow keys",
  purpose: "Focus the first tab of the staff console and move right and left with the arrow keys, checking the view on screen changes.",
  why: "Staff who cannot use a mouse must reach every view; the tabs are the only way between the parts of a console." }, async (t) => {
  // The next view along is the held balance workbench, which is not a hardship specialist's to read: the
  // view opens and the system refuses its contents, which is right and is not what this test is about.
  const s = await t.open("/staff", cast.st6, 403);
  await s.page.getByRole("tab", { name: "Policies", exact: true }).focus();
  await s.page.keyboard.press("ArrowRight");
  const selected = () => s.page.locator('[role="tab"][aria-selected="true"]').innerText();
  const second = await selected();
  if (second.trim() === "Policies") throw new Error("the right arrow did not move to the next view");
  await s.page.keyboard.press("ArrowLeft");
  if ((await selected()).trim() !== "Policies") throw new Error("the left arrow did not return to the first view");
  await s.sees("Find a customer or a policy");
});

test({ id: "E2E-026", area: A, persona: "ST5", story: "US-ST05-1", title: "A link to one view of a console opens that view",
  purpose: "Open the staff console at the address of its My access view and check that view, not the first, is on screen after sign-in.",
  why: "Staff send each other links to the exact place to look; a link that always opens the first tab wastes the hand-over." }, async (t) => {
  const s = await t.open("/staff#my-access", cast.st5);
  await s.sees("What you can do");
  const tab = await s.page.locator('[role="tab"][aria-selected="true"]').innerText();
  if (tab.trim() !== "My access") throw new Error(`the selected view is "${tab}"`);
  await s.sees("collections", "#mine");
});

test({ id: "E2E-027", area: A, persona: "ST6", story: "US-ST06-1", title: "The first key press offers a way past the navigation to the page",
  purpose: "Press Tab once on a console and follow the skip link to the page's content.",
  why: "A screen reader or keyboard user should not have to pass thirteen navigation links on every page load." }, async (t) => {
  const s = await t.open("/complaints", cast.st14);
  await s.page.evaluate(() => { document.activeElement && document.activeElement.blur(); window.scrollTo(0, 0); });
  const skip = s.page.locator("a.skip");
  await skip.focus();
  if ((await skip.innerText()).trim() !== "Skip to the page") throw new Error("the skip link is missing its words");
  await s.page.keyboard.press("Enter");
  if (!(await s.page.evaluate(() => window.location.hash === "#content" || document.activeElement.id === "content"))) throw new Error("the skip link did not move to the content");
});

test({ id: "E2E-028", area: A, persona: "Anyone", story: "US-ST16-2", title: "Every page is served with a strict content security policy and nothing from elsewhere",
  purpose: "Load every page of the portal and record each request the browser makes; all must be to the portal itself, and each page must carry a policy that forbids inline script from elsewhere.",
  why: "The consoles show health, money and identity data; a page that pulls script or fonts from another origin leaks who is looking at what and widens the attack surface." }, async (t) => {
  const s = await t.open("/", null);
  const elsewhere = [];
  s.page.on("request", (r) => { if (!r.url().startsWith(t.stack.portal) && !r.url().startsWith("data:") && !r.url().startsWith("blob:")) elsewhere.push(r.url()); });
  for (const page of ["", "status", "adviser", "employer", ...CONSOLES.map(([p]) => p)]) {
    const res = await s.page.goto(`${t.stack.portal}/${page}`, { waitUntil: "networkidle" });
    const csp = res.headers()["content-security-policy"] || "";
    if (!/default-src 'none'|default-src 'self'/.test(csp)) throw new Error(`/${page} has no strict content security policy: "${csp}"`);
    if (/unsafe-eval/.test(csp)) throw new Error(`/${page} allows eval`);
  }
  if (elsewhere.length) throw new Error(`requests left the portal: ${elsewhere.join(", ")}`);
});
