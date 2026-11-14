// The reinsurance console: the exceptions that need a person, the rule that
// abandoning anything takes a second person, and the month-end check.
//
// The reinsurance driver, which delivers the module's instructions to the
// core, is not part of this stack. The month-end check therefore reports
// instructions not delivered, and the tests read that as the true state.
"use strict";
const { test } = require("../lib/suite");
const { cast } = require("../lib/cast");

const A = "Reinsurance";
const BASE = "/staff/v1/reinsurance/";
/// An abandonment a member of reinsurance operations has prepared and nobody has yet signed.
async function prepared(t, reason) {
  const r = await t.does(cast.ri1, "POST", BASE + "commands", { abandon_notice: { seq: 1, reason } }, "prepares an abandonment");
  if (!r.prepared) throw new Error(`the abandonment was not held for a second person: ${JSON.stringify(r)}`);
  return r.prepared;
}
const withdraw = (t, id) => t.sys.portal("POST", BASE + "withdraw/" + encodeURIComponent(id), cast.ri1);

test({ id: "E2E-253", area: A, persona: "Reinsurance", story: "US-R01-1", title: "Reinsurance operations arrive able to act on exceptions but not to sign abandonments", matrix: ["RI-011"],
  purpose: "Sign in to the reinsurance console as a member of reinsurance operations.",
  why: "The console must say which half of the two-person rule this person holds, and the date their commands will carry." }, async (t) => {
  const s = await t.open("/reinsurance", cast.ri1);
  await s.sees("You are ri1: you act on exceptions.", "#me");
  await s.never("you sign abandonments", "#me");
  await s.sees("Commands are made on 20", "#me");
});

test({ id: "E2E-254", area: A, persona: "Reinsurance", story: "US-R01-1", title: "The reinsurance lead both acts on exceptions and signs abandonments", matrix: ["RI-011"],
  purpose: "Sign in as the reinsurance lead.",
  why: "Someone must be able to sign; the page must show that this person can." }, async (t) => {
  const s = await t.open("/reinsurance", cast.ri2);
  await s.sees("you act on exceptions, you sign abandonments", "#me");
});

test({ id: "E2E-255", area: A, persona: "Auditor", story: "US-R01-1", title: "The auditor reads the reinsurance exceptions and cannot act on one", matrix: ["RI-011"],
  purpose: "Sign in as the read-only auditor, then send a command by hand.",
  why: "Audit reads everything and changes nothing; the module must refuse the command, not only hide the buttons." }, async (t) => {
  const s = await t.open("/reinsurance", cast.auditor);
  await s.sees("view only", "#me");
  await s.sees("Open exceptions");
  const r = await t.sys.portal("POST", BASE + "commands", cast.auditor, { retry_notice: { seq: 1 } });
  if (r.status !== 403 || !String(r.data.detail).includes("hold no role to act")) throw new Error(`the command answered ${r.status} ${JSON.stringify(r.data)}`);
});

test({ id: "E2E-256", area: A, persona: "ST5", story: "US-R01-1", title: "Staff with no reinsurance role are refused the console", matrix: ["RI-011"],
  purpose: "Open the reinsurance console as a service officer.",
  why: "Treaty positions and reinsurer settlements are commercially sensitive; only the roles that work them see them." }, async (t) => {
  const s = await t.open("/reinsurance", cast.st5b, 403);
  await s.refused("do not include reinsurance");
});

test({ id: "E2E-257", area: A, persona: "Reinsurance", story: "US-R01-1", title: "An abandonment is held for a second person, and its preparer can only withdraw it", matrix: ["RI-014"],
  purpose: "Prepare the abandonment of a notice, read Waiting for a second person as its preparer, and withdraw it.",
  why: "Abandoning a notice means claims never hears of it; one person must not be able to do that alone, and must be able to take it back." }, async (t) => {
  const id = await prepared(t, t.ref("abandon"));
  const s = await t.open("/reinsurance", cast.ri1);
  await s.sees(id, "#pending");
  const buttons = await s.row("#pending", id).getByRole("button").allInnerTexts();
  if (buttons.join() !== "Withdraw") throw new Error(`the preparer is offered ${JSON.stringify(buttons)}`);
  await s.stale();
  await s.row("#pending", id).getByRole("button", { name: "Withdraw" }).click();
  await s.done("#p-msg", "Withdrawn.");
  await s.never(id, "#pending");
});

test({ id: "E2E-258", area: A, persona: "Reinsurance", story: "US-R01-1", title: "The preparer cannot sign their own abandonment", matrix: ["RI-014"],
  purpose: "As the preparer, send the signature for their own prepared abandonment.",
  why: "The second signature is the control; it is worthless if the first person can supply it." }, async (t) => {
  const id = await prepared(t, t.ref("abandon"));
  const r = await t.sys.portal("POST", BASE + "sign/" + encodeURIComponent(id), cast.ri1);
  if (r.status !== 409 || !String(r.data.detail).includes("someone else signs what you prepared")) throw new Error(`the signature answered ${r.status} ${JSON.stringify(r.data)}`);
  await withdraw(t, id);
});

test({ id: "E2E-259", area: A, persona: "Auditor", story: "US-R01-1", title: "Someone without the approval role cannot sign an abandonment", matrix: ["RI-014"],
  purpose: "As the auditor, send the signature for an abandonment someone else prepared.",
  why: "A second person is not enough; it must be a person the insurer has authorised to sign." }, async (t) => {
  const id = await prepared(t, t.ref("abandon"));
  const r = await t.sys.portal("POST", BASE + "sign/" + encodeURIComponent(id), cast.auditor);
  if (r.status !== 403 || !String(r.data.detail).includes("reinsurance.approve")) throw new Error(`the signature answered ${r.status} ${JSON.stringify(r.data)}`);
  await withdraw(t, id);
});

test({ id: "E2E-260", area: A, persona: "Reinsurance", story: "US-R01-1", title: "The lead signs a prepared abandonment and the module still checks it can be done", matrix: ["RI-014"],
  purpose: "As the lead, press Sign and send on an abandonment of a notice that was never refused.",
  why: "Two signatures authorise a command; they do not make a wrong one right. A notice that was never refused cannot be abandoned." }, async (t) => {
  const id = await prepared(t, t.ref("abandon"));
  const s = await t.open("/reinsurance", cast.ri2);
  await s.sees(id, "#pending");
  await s.stale();
  await s.row("#pending", id).getByRole("button", { name: "Sign and send" }).click();
  await s.stopped("#p-msg", "has not been refused", 409);
});

test({ id: "E2E-261", area: A, persona: "ST11", story: "US-ST11-1", title: "The month-end check says what in reinsurance stops the close", matrix: ["RI-011", "FIN-001"],
  purpose: "Run the month-end check for the current period on a system whose reinsurance instructions have not been delivered.",
  why: "The finance close asks reinsurance whether it is clean; the answer must name what is outstanding, not just say no." }, async (t) => {
  await t.policy("term_life", `cust-${t.ref("ri").toLowerCase()}`);
  const s = await t.open("/reinsurance", cast.ri1);
  await s.tab("Month-end check");
  await s.fill("#s-date", `${t.stack.opening.slice(0, 8)}30`);
  await s.press("Check", "#sweep");
  await s.sees("Stopping the close:", "#s-out");
  await s.sees("instructions not delivered", "#s-out");
});

test({ id: "E2E-262", area: A, persona: "Reinsurance", story: "US-R01-1", title: "Nothing is listed as abandoned on a system where nothing has been", matrix: ["RI-013"],
  purpose: "Read the Notices and abandoned view.",
  why: "The abandoned list is the permanent record of what was written off between the ledgers; it must be empty until two people have signed something into it." }, async (t) => {
  const s = await t.open("/reinsurance", cast.ri1);
  await s.tab("Notices and abandoned");
  await s.sees("Abandoned");
  let text = "";
  for (let i = 0; i < 40 && !text; i++) { text = await s.text("#abandoned"); if (!text) await new Promise((r) => setTimeout(r, 250)); }
  if (!/Instructions\s*None\./i.test(text.replace(/\n/g, " ")) || !/Notices\s*None\./i.test(text.replace(/\n/g, " "))) throw new Error(`the abandoned list reads "${text}"`);
});
