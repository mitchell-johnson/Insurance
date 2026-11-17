// The payments register as the payments team keeps it: what the screening and
// bank services report to the payments module, and an account that may be
// paid, entered by one payee maintainer, confirmed by another and screened
// clear. Claims pick their payees from this register (17 Nov 2026), so the
// claims journeys use it too.
"use strict";
const { cast } = require("./cast");

const COMMANDS = "/staff/v1/payments/commands";

/// What a service reports to the payments module (the bank driver, screening, the customer master).
async function reports(t, command, roles = ["service"]) {
  const r = await t.sys.tell("payments", { key: t.ref("svc"), actor: { id: "bank-driver", roles }, at: `${t.stack.opening}T10:00:00`, command });
  const inner = r.result && r.result.Ok;
  if (!inner || inner.Err) throw new Error(`payments refused ${command.type}: ${JSON.stringify(r).slice(0, 300)}`);
  return inner.Ok;
}
const sends = (t, who, command) => t.does(who, "POST", COMMANDS, command, command.type);

/// An account that may be paid: entered by one maintainer, confirmed by another, screened clear.
/// `party` is who it pays (a new party unless given) and `name` the account holder's name a person picks it by.
async function payee(t, { kind = "customer", confirm = true, screen = true, name_check = "match", party, name = "Account Holder" } = {}) {
  const id = t.ref("pay").toLowerCase();
  party = party || t.ref("cust").toLowerCase();
  // Each payee has its own account number: the duplicate check looks at the account and the amount, across runs.
  const number = `01-0001-${String(Date.now() % 10_000_000).padStart(7, "0")}-${String(Math.floor(Math.random() * 100)).padStart(2, "0")}`;
  await sends(t, cast.payee, { type: "register_payee", payee: id, party, kind, account: { name, number }, name_check });
  if (confirm) await sends(t, cast.payee2, { type: "confirm_payee", payee: id });
  if (confirm && screen) await reports(t, { type: "screen_payee", payee: id, clear: true });
  return { id, party, number, name };
}

module.exports = { COMMANDS, reports, sends, payee };
