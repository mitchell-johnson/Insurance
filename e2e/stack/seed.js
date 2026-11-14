// Seeds the running system with the persona cast and the configuration an
// insurer activates before its first day: staff roles and people, each
// module's rules, the policies the personas hold. Everything goes in through
// a route or a command the system offers for that purpose, as the named
// service or person who would send it; nothing is written into a store.
//
// Safe to run twice: every command carries a key, and a repeat changes
// nothing.
"use strict";
const path = require("path");
const { spawnSync } = require("child_process");
const { System } = require("./cluster");
const { cast, templates, profiles, staff, partners } = require("../lib/cast");

const root = path.resolve(__dirname, "../..");
const bin = path.resolve(root, process.env.IC_BIN || "target/debug");

function shipped() {
  const r = spawnSync(path.join(bin, "ic-e2e-config"), [], { encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (r.status !== 0) throw new Error(`ic-e2e-config failed: ${r.stderr}`);
  return JSON.parse(r.stdout);
}
const Journey_before = (date, days) => new Date(Date.parse(`${date}T00:00:00Z`) - days * 86400000).toISOString().slice(0, 10);
const refused = (reply) => reply && reply.result && reply.result.Err ? reply.result.Err : reply && reply.refused ? reply.refused : null;

async function seed(stack) {
  const sys = new System(stack);
  const config = shipped();
  // The tests issue policies from the shipped application templates.
  require("fs").writeFileSync(path.join(stack.run, "shipped.json"), JSON.stringify(config));
  const opening = stack.opening;
  const now = new Date().toISOString();
  const log = (line) => console.log(`seed: ${line}`);
  const told = async (module, envelope, what) => {
    const reply = await sys.tell(module, envelope);
    // The node's answer wraps the module's own: either can refuse.
    const no = reply.result && reply.result.Err ? reply.result.Err : refused(reply.result ? reply.result.Ok : reply);
    if (no) throw new Error(`${module} refused ${what}: ${JSON.stringify(no)}`);
    return reply.result ? reply.result.Ok : reply;
  };

  // --- Staff: the role templates, the first administrator, then everyone else as that administrator employs them.
  await sys.internal("entitlements", { staff: { bootstrap: { templates, profiles, admin: cast.admin.party, admin_templates: cast.admin.templates } } }, "seed-bootstrap");
  for (const person of [...staff, ...partners]) await sys.internal("identity", { directory: { bind_subject: { subject: person.subject, party: person.party } } }, `seed-bind-${person.subject}`);
  for (const person of staff.filter((p) => p.key !== "admin")) {
    const r = await sys.portal("POST", "/staff/v1/access", cast.admin, { employ: { party: person.party, team: "operations", manager: null, templates: person.templates } }, `seed-employ-${person.key}`);
    if (!r.ok && r.data.title !== "already_staff") throw new Error(`employ ${person.key}: ${r.status} ${JSON.stringify(r.data)}`);
  }
  log(`${staff.length} staff employed, ${partners.length} partners and customers bound`);

  // --- Complaints: the timers and limits of the conduct regime.
  await told("complaints", { key: "seed-timers-1", caller: "config", actor: "config", at: now, command: { activate_timers: {
    version: 1, acknowledge_days: 5, update_every_days: 10, decide_days: 20, deadlock_days: 40, warning_days: 2, conduct_officer_after_days: 3, complainant_response_days: 10,
    vulnerable_extra_days: 10, front_line_limit_minor: 100_00, ex_gratia_limits_minor: { 1: 500_00, 3: 5_000_00 }, regulator_notification_days: 10, retain_closed_days: 2555,
    cause_codes: ["delay", "wrong_decision", "poor_explanation", "adviser_conduct", "billing_error", "privacy_breach"] } } }, "the timers");
  log("complaints timers active");

  // --- Finance: the shipped chart and close configuration, proposed by one set-up person and approved by another.
  const fin = (key, caller, actor, role, command) => told("finance", { key, caller, ...(actor ? { actor, roles: [role] } : {}), command }, key);
  await fin("seed-configure", "operator", null, null, { configure: { partitions: stack.partitions, year_end_month: 3 } });
  await fin("seed-chart", "finance-console", "config-author", "finance_operator", { propose_chart: { chart: config.finance.chart } });
  await fin("seed-chart-ok", "finance-console", "config-approver", "finance_lead", { approve_config: { id: config.finance.chart_id } });
  await fin("seed-close", "finance-console", "config-author", "finance_operator", { propose_close_config: { config: config.finance.close } });
  await fin("seed-close-ok", "finance-console", "config-approver", "finance_lead", { approve_config: { id: config.finance.close_id } });
  log("finance chart and close configuration active");

  // --- Claims: the rules and the authority book, on every partition.
  const all = ["reserve", "admit", "decline", "withdraw", "end_benefit"];
  const rules = {
    version: 1, escalation_days: [1, 3, 7], large_claim_minor: 1_000_000_00, two_signature_admission_minor: 250_000_00, two_signature_ex_gratia_minor: 5_000_00, two_signature_write_off_minor: 1_000_00,
    sharp_reserve_move_pct: 50, reason_codes: { "R-OK": all, "R-EVID": all, "R-EXCL": ["decline"], "R-NDISC": ["decline"], "R-WDRAW": ["withdraw"], "R-RECOV": ["end_benefit"] },
    cause_codes: { "CC-0003": ["cancer.skin"], "CC-0101": ["cardiovascular.heart_attack"], "CC-0204": ["musculoskeletal.back"], "CC-0900": ["injury.accident"] },
    medical_review_months: 6, income_declaration_months: 3, review_grace_days: 14, max_offset_pct: 25, partitions: stack.partitions, late_death_days: 30, early_claim_days: 7,
    funeral_advance_max_minor: 15_000_00, late_interest: { annual_rate_bps: 500, free_days: 30 }, estate_simplified_max_minor: 15_000_00, pre_approval_days: 90, recurrence_days: 180,
    health_benefits: { surgical: { per_event_minor: 50_000_00, per_year_minor: 200_000_00, lifetime_minor: 2_000_000_00, copay_pct: 0, excess_minor: 500_00 } },
    prepared_by: "config-author", approved_by: ["config-approver", "config-second"],
  };
  const types = ["death", "terminal_illness", "trauma", "total_permanent_disability", "income_protection", "health", "premium_waiver"];
  const limit = (amount) => Object.fromEntries(types.map((t) => [t, amount]));
  const queues = ["health", "lump_sum", "income_protection_new", "income_protection_ongoing", "group", "complex_large", "medical_officer", "rehabilitation", "recoveries", "quality_review"];
  const person = (profile, roles, extra = {}) => ({ profile, roles, queues, wip_limit: 0, vulnerable_trained: false, related_parties: [], ...extra });
  const book = {
    version: 1,
    profiles: {
      "health-assessor": { lines: ["health"], max_admission_minor: { health: 5_000_00 }, max_payment_minor: 5_000_00, max_reserve_minor: 10_000_00, may_decline: false },
      "senior-assessor": { lines: ["health", "lump_sum", "income_protection"], max_admission_minor: limit(600_000_00), max_payment_minor: 600_000_00, max_reserve_minor: 1_000_000_00, may_decline: true,
        max_ex_gratia_minor: 5_000_00, max_write_off_minor: 1_000_00, may_reverse_admission: true, may_change_recovery_status: true, senior: true },
    },
    staff: {
      [cast.st3.party]: person("health-assessor", ["assessor"]),
      [cast.st4.party]: person("senior-assessor", ["assessor", "queue_lead"], { vulnerable_trained: true }),
      [cast.st4b.party]: person("senior-assessor", ["assessor", "claims_manager"]),
      [cast.n7.party]: person("senior-assessor", ["assessor"], { related_parties: ["cust-p60"] }),
      [cast.st13.party]: person("senior-assessor", ["investigator"]),
    },
    prepared_by: "config-author", approved_by: ["config-approver", "config-second"],
  };
  for (const [key, command] of [["seed-claims-rules", { claims: { activate_rules: { rules } } }], ["seed-claims-book", { claims: { activate_authority: { book } } }]]) {
    const r = await sys.http("POST", stack.api, "/v1/commands", { headers: { "x-dev-subject": "svc-config", "x-dev-roles": "config", "idempotency-key": key }, body: { command } });
    if (!r.ok || (r.data && r.data.rejected)) throw new Error(`claims configuration ${key}: ${r.status} ${JSON.stringify(r.data)}`);
  }
  log("claims rules and authority book active on every partition");
  // --- Payments: the banks, channels, accounts, routes and control rules. No bank is named in the design, so these are
  // two stand-in banks; the pump that talks to a bank is not part of this stack, and tests report its part as the driver.
  const regions = ["akl", "syd", "mel"];
  const endpoint = (channel, region) => ({ url: `https://${channel}.bank.test/${region}`, credential: `${channel}-${region}-credential`, signing_key: `${channel}-${region}-signing`,
    certificate_expires: "2028-06-30", egress_address: "203.0.113.3", whitelisted: true });
  const channel = (id, bank, kind, hour) => [id, { bank, kind, cutoff: `${hour}:00:00`, endpoints: Object.fromEntries(regions.map((r) => [r, endpoint(id, r)])) }];
  const purposes = { collection: "premium_collection", claims: "claims_disbursement", third: "third_party_disbursement", refunds: "refunds", operating: "operating" };
  const accounts = {};
  for (const bank of ["anz", "bnz"]) for (const [code, purpose] of Object.entries(purposes)) accounts[`${bank}-sf1-${code}`] = { bank, fund: "sf1", purpose, currency: "NZD" };
  const runTypes = ["collection", "lump_sum_claims", "benefit_payments", "provider_payments", "premium_refunds", "commission", "reinsurer", "fund_settlement", "tax_and_levies", "unclaimed_money", "fund_transfer", "ex_gratia"];
  const payments = {
    banks: { anz: "ANZ", bnz: "BNZ" },
    channels: Object.fromEntries([channel("anz-file", "anz", "file", 16), channel("anz-api", "anz", "api", 22), channel("bnz-file", "bnz", "file", 15)]),
    accounts,
    routes: Object.fromEntries(runTypes.map((t) => [t, { primary: "anz-file", secondary: "bnz-file", urgent: "anz-api", approvals: t === "ex_gratia" ? 2 : 1,
      item_limit_minor: 100_000_00, run_limit_minor: 500_000_00, day_limit_minor: 800_000_00 }])),
    funds: ["sf1"],
    rules: { cooling_off_hours: 24, duplicate_window_days: 3, dishonours_before_suspension: 2, pre_advice_threshold_minor: 250_000_00, on_call: [cast.st17.party],
      drill_max_age_days: 90, drill_account: { name: "Insurer Operating", number: "0100000001" }, drill_amount_minor: 1_00, voter_regions: regions, holidays: ["2027-12-27", "2027-12-28"] },
  };
  await told("payments", { key: "seed-payments-config-1", actor: { id: "connectivity-admin", roles: ["connectivity_admin"] }, at: `${opening}T08:00:00`, command: { type: "activate_config", version: 1, config: payments } }, "its configuration");
  // The bank driver reads a statement for every account each morning; without one no run can be approved, because the balance is unknown.
  for (const code of Object.keys(accounts)) await told("payments", { key: `seed-statement-${code}`, actor: { id: "bank-driver", roles: ["service"] }, at: `${opening}T08:05:00`,
    command: { type: "record_statement", account: code, date: Journey_before(opening, 3), opening_minor: 10_000_000_00, closing_minor: 10_000_000_00, lines: [] } }, `the statement of ${code}`);
  log("payments banks, channels, routes and rules active; a statement read for every account");
}


module.exports = { seed, shipped };

if (require.main === module) {
  const fs = require("fs");
  const run = path.resolve(root, process.env.IC_E2E_RUN || "e2e/.run");
  seed(JSON.parse(fs.readFileSync(path.join(run, "stack.json"), "utf8"))).then(() => console.log("seeded")).catch((e) => { console.error(String(e.stack || e)); process.exit(1); });
}
