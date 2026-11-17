// Seeds the running system with the persona cast and the configuration an
// insurer activates before its first day: staff roles and people, each
// module's rules, the policies the personas hold. Each module's configuration
// is written as the set-up file an operator keeps and is sent by the
// operator's own tool, `ic-admin apply`. Everything else goes in through a
// route or a command the system offers for that purpose, as the named service
// or person who would send it; nothing is written into a store.
//
// Safe to run twice: every command carries a key, and a repeat changes
// nothing.
"use strict";
const fs = require("fs");
const path = require("path");
const { spawnSync } = require("child_process");
const { System } = require("./cluster");
const { cast, templates, profiles, staff, partners, starters } = require("../lib/cast");

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
  fs.writeFileSync(path.join(stack.run, "shipped.json"), JSON.stringify(config));
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
  for (const person of [...staff, ...partners, ...starters]) await sys.internal("identity", { directory: { bind_subject: { subject: person.subject, party: person.party } } }, `seed-bind-${person.subject}`);
  for (const person of staff.filter((p) => p.key !== "admin")) {
    const r = await sys.portal("POST", "/staff/v1/access", cast.admin, { employ: { party: person.party, team: "operations", manager: null, templates: person.templates } }, `seed-employ-${person.key}`);
    if (!r.ok && r.data.title !== "already_staff") throw new Error(`employ ${person.key}: ${r.status} ${JSON.stringify(r.data)}`);
  }
  // Leilani (N7) is related to the customer cust-p60 and has said so, as staff do from the staff console.
  const declared = await sys.portal("POST", "/staff/v1/access", cast.n7, { declare_connection: { party: "cust-p60" } }, "seed-n7-connection");
  if (!declared.ok) throw new Error(`n7 declares her connection: ${declared.status} ${JSON.stringify(declared.data)}`);
  log(`${staff.length} staff employed, ${partners.length} partners and customers bound, ${starters.length} starter bound and not employed`);

  // --- The insurer's own configuration, as one set-up file sent by the operator's tool, `ic-admin apply`
  // (`docs/operations.md`, step 11): a real deployment is configured the same way, with its own file.
  const steps = [];
  const configure = (module, envelope) => steps.push({ module: { module, envelope, accept: [] } });

  // Complaints: the timers and limits of the conduct regime.
  configure("complaints", { key: "seed-timers-1", caller: "config", actor: "config", at: now, command: { activate_timers: {
    version: 1, acknowledge_days: 5, update_every_days: 10, decide_days: 20, deadlock_days: 40, warning_days: 2, conduct_officer_after_days: 3, complainant_response_days: 10,
    vulnerable_extra_days: 10, front_line_limit_minor: 100_00, ex_gratia_limits_minor: { 1: 500_00, 3: 5_000_00 }, regulator_notification_days: 10, retain_closed_days: 2555,
    cause_codes: ["delay", "wrong_decision", "poor_explanation", "adviser_conduct", "billing_error", "privacy_breach"] } } });

  // Finance: the shipped chart and close configuration, proposed by one set-up person and approved by another.
  const fin = (key, caller, actor, role, command) => configure("finance", { key, caller, ...(actor ? { actor, roles: [role] } : {}), command });
  fin("seed-configure", "operator", null, null, { configure: { partitions: stack.partitions, year_end_month: 3 } });
  fin("seed-chart", "finance-console", "config-author", "finance_operator", { propose_chart: { chart: config.finance.chart } });
  fin("seed-chart-ok", "finance-console", "config-approver", "finance_lead", { approve_config: { id: config.finance.chart_id } });
  fin("seed-close", "finance-console", "config-author", "finance_operator", { propose_close_config: { config: config.finance.close } });
  fin("seed-close-ok", "finance-console", "config-approver", "finance_lead", { approve_config: { id: config.finance.close_id } });

  // Claims: the rules and the authority book, on every partition.
  const all = ["reserve", "admit", "decline", "withdraw", "end_benefit"];
  // The decisions a claim can make with an ordinary reason: what an assessor records on a funeral advance, a built-in
  // benefit, an ex gratia payment, a benefit suspended in custody and resumed, a write-off and a recovery's status.
  const ordinary = [...all, "funeral_advance", "admit_built_in", "ex_gratia", "suspend_benefit", "resume_benefit", "write_off", "recovery_status", "permanent_disablement"];
  const rules = {
    version: 1, escalation_days: [1, 3, 7], large_claim_minor: 1_000_000_00, two_signature_admission_minor: 250_000_00, two_signature_ex_gratia_minor: 5_000_00, two_signature_write_off_minor: 1_000_00,
    sharp_reserve_move_pct: 50, reason_codes: { "R-OK": ordinary, "R-EVID": ordinary, "R-EXCL": ["decline"], "R-NDISC": ["decline", "reverse_admission"], "R-WDRAW": ["withdraw"], "R-RECOV": ["end_benefit"] },
    cause_codes: { "CC-0003": ["cancer.skin"], "CC-0101": ["cardiovascular.heart_attack"], "CC-0204": ["musculoskeletal.back"], "CC-0900": ["injury.accident"] },
    medical_review_months: 6, income_declaration_months: 3, review_grace_days: 14, max_offset_pct: 25, partitions: stack.partitions, late_death_days: 30, early_claim_days: 7,
    non_disclosure_reasons: ["R-NDISC"], funeral_advance_max_minor: 15_000_00, late_interest: { annual_rate_bps: 500, free_days: 30 }, estate_simplified_max_minor: 15_000_00, pre_approval_days: 90, recurrence_days: 180,
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
      // The claims manager decides what is beyond a senior assessor: the shipped products insure up to $2,000,000
      // of trauma and more of life, and a claim nobody may admit would stop at the senior's limit.
      "claims-manager": { lines: ["health", "lump_sum", "income_protection"], max_admission_minor: limit(5_000_000_00), max_payment_minor: 5_000_000_00, max_reserve_minor: 5_000_000_00, may_decline: true,
        max_ex_gratia_minor: 25_000_00, max_write_off_minor: 10_000_00, may_reverse_admission: true, may_change_recovery_status: true, senior: true },
    },
    staff: {
      [cast.st3.party]: person("health-assessor", ["assessor"]),
      [cast.st4.party]: person("senior-assessor", ["assessor", "queue_lead"], { vulnerable_trained: true }),
      [cast.st4b.party]: person("claims-manager", ["assessor", "claims_manager"]),
      [cast.n7.party]: person("senior-assessor", ["assessor"], { related_parties: ["cust-p60"] }),
      [cast.st13.party]: person("senior-assessor", ["investigator"]),
    },
    prepared_by: "config-author", approved_by: ["config-approver", "config-second"],
  };
  steps.push({ core: { key: "seed-claims-rules", command: { claims: { activate_rules: { rules } } } } });
  steps.push({ core: { key: "seed-claims-book", command: { claims: { activate_authority: { book } } } } });

  // Payments: the banks, channels, accounts, routes and control rules. No bank is named in the design, so these are
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
  configure("payments", { key: "seed-payments-config-1", actor: { id: "connectivity-admin", roles: ["connectivity_admin"] }, at: `${opening}T08:00:00`, command: { type: "activate_config", version: 1, config: payments } });

  // Underwriting: the shipped libraries (exclusions, reasons, both question sets, the individual rule set) and an
  // authority profile for each underwriter from the shipped grades. Each change has its author and two other approvers,
  // none of them the underwriter whose authority it is. The juniors hold the assistant grade (no loading, $500,000 of
  // life cover alone), the seniors the senior grade.
  const uw = config.underwriting;
  const approval = (change) => ({ author: "config-author", approvers: ["config-approver", "config-second"], change_ref: change });
  const underwrite = (key, command) => configure("underwriting", { key, caller: "config", at: `${opening}T08:15:00Z`, today: opening, command });
  underwrite("seed-uw-exclusions", { activate_exclusions: { library: uw.exclusions, approval: approval("seed-uw-libraries") } });
  underwrite("seed-uw-reasons", { activate_reasons: { library: uw.reasons, approval: approval("seed-uw-libraries") } });
  for (const set of uw.question_sets) underwrite(`seed-uw-questions-${set.id}-${set.version}`, { activate_question_set: { set, approval: approval("seed-uw-libraries") } });
  for (const set of uw.rule_sets) underwrite(`seed-uw-rules-${set.id}-${set.version}`, { activate_rule_set: { set, backtest_digest: null, approval: approval("seed-uw-libraries") } });
  const grade = (id) => { const g = uw.grades.find((x) => x.underwriter === id); if (!g) throw new Error(`no shipped underwriting grade ${id}`); return g; };
  for (const [who, held] of [[cast.st1, "grade-assistant"], [cast.st1b, "grade-assistant"], [cast.st2, "grade-senior"], [cast.st2b, "grade-senior"]]) {
    underwrite(`seed-uw-profile-${who.key}`, { activate_profile: { profile: { ...grade(held), underwriter: who.party }, approval: approval("seed-uw-profiles") } });
  }

  // Reinsurance: the shipped treaties and the reinsurers' rate table, so new business reserves capacity under a treaty
  // and cover above the retention is ceded.
  const ri = config.reinsurance;
  const reinsure = (key, command) => configure("reinsurance", { key, caller: "config", on: opening, command });
  for (const table of ri.rate_tables) reinsure(`seed-ri-rates-${table.id}-${table.version}`, { activate_rate_table: { table } });
  for (const treaty of ri.treaties) reinsure(`seed-ri-treaty-${treaty.id}-${treaty.version}`, { activate_treaty: { treaty } });
  for (const treaty of ri.aggregate_treaties) reinsure(`seed-ri-aggregate-${treaty.id}-${treaty.version}`, { activate_aggregate_treaty: { treaty } });

  // Unit pricing: how many partitions report units on issue.
  configure("unit-pricing", { key: "seed-pricing-partitions", caller: "unit-pricing-intake", actor: "unit-pricing-intake", roles: [], at: `${opening}T08:10:00Z`, command: { record_partitions: { count: stack.partitions } } });

  const setup = path.join(stack.run, "setup.json");
  fs.writeFileSync(setup, JSON.stringify({ steps }, null, 1));
  const applied = spawnSync(path.join(bin, "ic-admin"), ["apply", setup], { encoding: "utf8",
    env: { PATH: process.env.PATH, IC_NODES: stack.nodes.map((u) => u.replace(/^https?:\/\//, "")).join(","), IC_CLUSTER_NODES: stack.nodes.map((u) => u.replace(/^https?:\/\//, "")).join(","), IC_CLUSTER_TOKEN: stack.token, IC_LEDGER_PATIENCE_SECS: "120" } });
  if (applied.status !== 0) throw new Error(`ic-admin apply ${setup} failed: ${applied.stderr || applied.stdout}`);
  log(`ic-admin apply: ${applied.stdout.replace(/\s+/g, " ").trim()} (complaints timers, finance chart and close, claims rules and authority book, payments banks and routes, underwriting libraries and profiles, reinsurance treaties, unit pricing partitions)`);

  // The bank driver reads a statement for every account each morning; without one no run can be approved, because the balance is unknown.
  for (const code of Object.keys(accounts)) await told("payments", { key: `seed-statement-${code}`, actor: { id: "bank-driver", roles: ["service"] }, at: `${opening}T08:05:00`,
    command: { type: "record_statement", account: code, date: Journey_before(opening, 3), opening_minor: 10_000_000_00, closing_minor: 10_000_000_00, lines: [] } }, `the statement of ${code}`);
  log("the bank driver's part: a statement read for every account");

  // --- Distribution: two advice providers, the advisers engaged with them, and a current licence for each provider.
  // Entered by the distribution administrator through the commission console's own route.
  const structured = async (command, what) => {
    const r = await sys.portal("POST", "/staff/v1/commission/commands", cast.dist, { command, purpose: "setting up the distribution structure" });
    // A party or engagement already in the structure (the seed run again on a live stack) is as wanted.
    if (!r.ok && r.status !== 409) throw new Error(`${what}: ${r.status} ${JSON.stringify(r.data)}`);
  };
  const providers = [[cast.d6, "FSPR-6", [cast.d1, cast.d2]], [cast.d9, "FSPR-9", [cast.d9a]], [cast.d7, "FSPR-12", [cast.d4, cast.d5]]];
  // An engagement is accredited product by product (the commission module writes an adviser's business only for a
  // product the engagement names): every retail product an adviser sells here.
  const accreditations = ["term-life", "income-protection", "health-plan", "health-flex", "invest-plan", "aia-living", "aia-living-business", "aia-start-up-income",
    "aia-cover-for-life", "aia-essentials-life", "aia-starter", "aia-private-health", "aia-cancer-care"];
  for (const [provider, register, advisers] of providers) {
    await structured({ add_party: { party: provider.party, kind: "fap" } }, `provider ${provider.key}`);
    await structured({ record_licence: { party: provider.party, state: "active", effective: "2020-01-01", source_ref: register } }, `licence of ${provider.key}`);
    for (const adviser of advisers) {
      await structured({ add_party: { party: adviser.party, kind: "adviser" } }, `adviser ${adviser.key}`);
      await structured({ start_engagement: { id: `eng-${adviser.key}`, engagement: { adviser: adviser.party, with: provider.party, from: "2025-01-01", to: null, accreditations } } }, `engagement of ${adviser.key}`);
    }
  }
  // Northlight is a dealer group above Harbourline: it sees its providers' books, and none of its own.
  await structured({ add_party: { party: cast.d10.party, kind: "group" } }, "dealer group d10");
  await structured({ relate: { child: cast.d6.party, parent: cast.d10.party, kind: "under", from: "2025-01-01", to: null } }, "Harbourline under Northlight");
  log("three advice providers licensed, five advisers engaged, Harbourline under the Northlight dealer group");

  // --- Commission: a book for each engagement, the rule that shares what it earns, the starter schedules and which
  // schedule each product's new business is written under. One change set, proposed by commission operations and
  // approved by a commission approver and by compliance, through the commission console's route as each of them.
  // Each book pays its adviser 75% and the provider 25%, and the provider bears the clawback (the worked example of
  // D1 at D6 in `docs/testing/personas/personas-insurance.md`); a clawback an alteration sets off follows the
  // engagement that submitted it. The schedules are the placeholder AIA ones (`config/distribution/aia-schedules.json`),
  // and the stack's own retail products are written under the AIA schedule for the same kind of cover.
  const commission = async (who, command, key, what) => {
    const r = await sys.portal("POST", "/staff/v1/commission/commands", who, { command, purpose: "setting up commission under the distribution agreements" }, key);
    if (!r.ok) throw new Error(`${what}: ${r.status} ${JSON.stringify(r.data)}`);
    return r.data;
  };
  const schedules = JSON.parse(fs.readFileSync(path.join(root, "config/distribution/aia-schedules.json"), "utf8"));
  const changes = [...schedules.changes];
  for (const [product, schedule] of [["term-life", "aia-risk"], ["income-protection", "aia-income"], ["health-plan", "aia-health"], ["health-flex", "aia-health"]]) {
    changes.push({ set_product_schedule: { product, cover_type: null, schedule } });
  }
  // A rule may be backdated a year at most: the books start a year before the opening date, before any policy here.
  const from = Journey_before(opening, 365);
  const books = [];
  for (const [provider, , advisers] of providers) {
    for (const adviser of advisers) {
      const book = `book-${adviser.key}`;
      books.push(book);
      changes.push({ create_book: { id: book, name: `${adviser.about.split(",")[0]} at ${provider.about.split(",")[0]}`, engagement: `eng-${adviser.key}`, owner: provider.party, house: false } });
      const line = (payee, ppm, absorbs) => ({ payee, earner: null, component: "base", commission_type: null, share: { of_remainder: ppm }, from, until: null, disputed: false, absorbs_rounding: absorbs });
      changes.push({ activate_rule: { book, version: 1, effective_from: from, lines: [line(adviser.party, 750_000, true), line(provider.party, 250_000, false)],
        liability: [{ component: "base", liable: provider.party, share: 1_000_000, guarantor: null, from, until: null }], fragments: [], clawback_by_cause: true,
        examples: [{ name: "initial commission of $1,000 shared 75 and 25", on: from, commission_type: "initial", gross_minor: 1_000_00, premium_minor: 0,
          expect: [[adviser.party, "base", 750_00], [provider.party, "base", 250_00]] }],
        instructions_ref: `seed-agreement-${adviser.key}` } });
    }
  }
  // The payees the books pay, registered by the distribution administrator first, because a rule names only payees
  // with a current agreement; each but Kōwhai with an account. Kōwhai's account is left for the scenario that adds
  // it, so what its advisers' books earn for it is held until then.
  const payees = [cast.d6, cast.d1, cast.d2, cast.d7, cast.d4, cast.d5, cast.d9, cast.d9a];
  for (const payee of payees) {
    await commission(cast.dist, { register_payee: { party: payee.party, agreement_ref: `seed-agreement-${payee.key}`, advance_recovery_ppm: 0 } }, `seed-payee-${payee.key}`, `register ${payee.key} as a payee`);
    if (payee === cast.d7) continue;
    await commission(cast.dist, { add_payee_account: { party: payee.party, account_ref: `acct-${payee.party}`, name_check_ref: `name-check-${payee.party}`, step_up_ref: `step-up-${payee.party}`, appointment_ref: null } },
      `seed-payee-account-${payee.key}`, `add the account of ${payee.key}`);
  }
  // The change set is named by its content, so a seed run again proposes the same set once and a changed set anew.
  const set = `seed-commission-${require("crypto").createHash("sha256").update(JSON.stringify(changes)).digest("hex").slice(0, 12)}`;
  await commission(cast.comops, { propose_change: { id: set, changes, instructions_ref: "config/distribution/aia-schedules.json" } }, `${set}-propose`, "propose the commission set-up");
  await commission(cast.comapp, { approve_change: { id: set, compliance: false } }, `${set}-approve`, "approve the commission set-up");
  await commission(cast.st13, { approve_change: { id: set, compliance: true } }, `${set}-compliance`, "approve the commission set-up for compliance");
  log(`commission: ${books.length} books with their rules, ${schedules.changes.filter((c) => c.activate_schedule).length} schedules, each retail product's schedule named, ${payees.length} payees (all but Kōwhai with an account)`);

  // --- Collections: each product's dunning sequence, set from the shipped best estimate by the collections
  // configuration person through the collections console's routes, before any day is closed.
  const sequences = JSON.parse(fs.readFileSync(path.join(root, "config/collections/sequences.json"), "utf8")).sequences;
  for (const { product } of sequences) {
    const shipped = await sys.portal("GET", `/staff/v1/collections/shipped_sequence?product=${encodeURIComponent(product)}`, cast.st5c);
    if (!shipped.ok) throw new Error(`the shipped sequence for ${product}: ${shipped.status} ${JSON.stringify(shipped.data)}`);
    const set = await sys.portal("POST", "/staff/v1/collections/commands", cast.st5c, { type: "set_sequence", sequence: shipped.data }, `seed-sequence-${product}`);
    if (!set.ok) throw new Error(`set the sequence for ${product}: ${set.status} ${JSON.stringify(set.data)}`);
  }
  log(`collections: the dunning sequences of ${sequences.length} products set`);

  // --- Group schemes: one standing scheme for Kai Fresh Supermarkets, registered by group administration, with
  // its founding member and its first administrator. The tests that change a scheme register their own.
  if (stack.group) {
    await require("../lib/group").register(sys, "kaifresh");
    await require("../lib/group").register(sys, "southline", { employer: cast.e3 });
    log("the Kai Fresh and Southline Labour Hire schemes are registered with their file layouts and administrators");
  }
}


module.exports = { seed, shipped };

if (require.main === module) {
  const run = path.resolve(root, process.env.IC_E2E_RUN || "e2e/.run");
  seed(JSON.parse(fs.readFileSync(path.join(run, "stack.json"), "utf8"))).then(() => console.log("seeded")).catch((e) => { console.error(String(e.stack || e)); process.exit(1); });
}
