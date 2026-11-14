// A group scheme, set up the way the insurer sets one up: new business issues
// the scheme's policy on the core, group administration registers the scheme
// and the layout of the employer's membership file through the group console's
// own routes, and names the employer's first administrator.
//
// The seed uses it for the standing scheme (`kaifresh`) and the tests for the
// schemes they change. The terms, product and file layout are the shipped
// ones, printed by `ic-e2e-config` (`shipped.json`, under `group`).
"use strict";
const fs = require("fs");
const path = require("path");
const { cast } = require("./cast");

const COMMANDS = "/staff/v1/group/commands";
const HEAD = "Emp No,First,Last,DOB,Category,Salary,Site";
const pause = (ms) => new Promise((done) => setTimeout(done, ms));

/// Register scheme `id` for an employer, with its founding member E001 (staff, 60,000, Auckland).
/// Returns `{ id, policy, number }`.
async function register(sys, id, { employer = cast.e1, by = cast.st7, admin = true } = {}) {
  const g = JSON.parse(fs.readFileSync(path.join(sys.stack.run, "shipped.json"), "utf8")).group;
  const policy = `pol-${id}`, number = `WPL-${id.toUpperCase()}`, party = `party-${id}-e001`;
  const issue = JSON.parse(JSON.stringify(g.issue));
  Object.assign(issue, { policy, number, group: `grp-${id}`, policyholder: employer.party });
  issue.lives[0].kind.party = party;
  const issued = await sys.must(sys.api("POST", "/v1/policies", issue), `issue the policy of scheme ${id}`);
  if (issued.status !== "applied") throw new Error(`issue the policy of scheme ${id}: ${JSON.stringify(issued)}`);
  const tell = (command, what) => sys.must(sys.portal("POST", COMMANDS, by, command, `scheme-${id}-${what}`), `${by.key} ${what} for scheme ${id}`);
  await tell({ register_scheme: { scheme: id, policy, group: issue.group, employer: employer.party, inception: issue.inception, product: g.product, terms: g.terms, founding: [{ ...g.founder, party }], pools: [] } }, "register_scheme");
  await tell({ set_mapping: { scheme: id, mapping: g.mapping } }, "set_mapping");
  const scheme = { id, policy, number, mapping: g.mapping };
  if (admin) await firstAdministrator(sys, scheme, employer, by);
  return scheme;
}

/// Group administration names the scheme's first administrator, by the route the console's form uses. The portal
/// must first have heard of the policy from the core's feed.
async function firstAdministrator(sys, scheme, who = cast.e1, by = cast.st7) {
  for (let n = 0; ; n++) {
    const r = await sys.portal("PUT", `/staff/v1/group/schemes/${scheme.policy}/first-admin`, by, { party: who.party }, `scheme-${scheme.id}-admin`);
    if (r.ok) return;
    if (n >= 60 || r.status !== 404) throw new Error(`name the first administrator of scheme ${scheme.id}: ${r.status} ${JSON.stringify(r.data)}`);
    await pause(250);
  }
}

/// A membership file in the layout the schemes are registered with. A row is
/// `[employee, given, family, born (dd/mm/yyyy), category, annual salary in dollars, site]`.
const csv = (rows) => [HEAD, ...rows.map((r) => r.join(","))].join("\n") + "\n";

/// The scan result Defender for Storage would write on a file in quarantine.
const scan = (sys, scheme, file, clean = true) =>
  sys.must(sys.http("PUT", sys.stack.azure, "/__tags", { body: { key: `group-quarantine/${scheme.id}/${file}`, set: { "Malware Scanning scan result": clean ? "No threats found" : "Malicious" } } }), `scan result for ${file}`);

/// The group service's record of a file, as group administration reads it.
const file = (sys, scheme, name) => sys.portal("GET", `/staff/v1/group/file?scheme=${scheme.id}&file=${name}`, cast.st7);

/// Wait until a file reaches one of `statuses`; the group driver works once a second.
async function reaches(sys, scheme, name, ...statuses) {
  let last;
  for (let n = 0; n < 120; n++) {
    last = await file(sys, scheme, name);
    if (last.ok && statuses.includes(last.data.status)) return last.data;
    await pause(250);
  }
  throw new Error(`file ${name} of scheme ${scheme.id} never became ${statuses.join(" or ")}: ${JSON.stringify(last.data).slice(0, 300)}`);
}

/// The employer's administrator sends a file by the portal's upload route (what the page's Upload button does).
async function upload(sys, scheme, name, rows, who = cast.e1) {
  return sys.http("POST", sys.stack.portal, `/employer/v1/schemes/${scheme.policy}/group/files/${name}`, { headers: { "x-dev-subject": who.subject, "content-type": "text/csv" }, body: csv(rows), raw: true });
}

/// A file taken all the way: uploaded, scanned clean, previewed, approved by the employer and applied.
async function applied(sys, scheme, name, rows, who = cast.e1) {
  await sys.must(upload(sys, scheme, name, rows, who), `upload ${name}`);
  await scan(sys, scheme, name);
  await reaches(sys, scheme, name, "awaiting_approval");
  await sys.must(sys.portal("POST", `/employer/v1/schemes/${scheme.policy}/group/commands`, who, { approve_file: { file: name } }), `approve ${name}`);
  return reaches(sys, scheme, name, "applied");
}

module.exports = { register, firstAdministrator, csv, scan, file, reaches, upload, applied, HEAD };
