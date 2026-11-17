// The parts of the world outside the insurer's screens that a scenario operator
// plays on the running stack, each as the service that would really send it,
// through the route or module command that service uses. Nothing here acts for
// a person: every person's decision is made on a screen or through /mcp.
//
//   node e2e/stack/play.js propose  '<json>'   new business opens a proposal for underwriting
//   node e2e/stack/play.js proposal '<json>'   what underwriting holds on a proposal: lives, cases, evidence asked for
//   node e2e/stack/play.js evidence '<json>'   an evidence provider reports on an order (acknowledged, received, unobtainable)
//   node e2e/stack/play.js issue    '<json>'   new business issues a policy straight from an application, and says who advised it
//   node e2e/stack/play.js advise   '<json>'   new business tells commission who advised a policy already issued
//
// No channel opens a proposal yet (`docs/underwriting.md`): the adviser portal
// has no application, and the new business officer's `quote` tool and route
// price an application without keeping it. New business is therefore played
// here, by the command its service sends: `open_proposal` to the underwriting
// module on the ledger nodes, as the caller `new-business`, with the quote the
// core gives for the application. The evidence provider is played through the
// underwriting intake's own route, `POST /v1/evidence`, with a token for the
// stand-in provider's app registration, which holds the `evidence` role.
//
// Each command prints what it did as JSON. `<json>` may also be `@file.json`.
// The stack is the one `IC_E2E_RUN` names (default `e2e/.run`). See e2e/README.md,
// "Playing new business and an evidence provider", for worked examples.
"use strict";
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");
const { System } = require("./cluster");
const { token } = require("../lib/customer");

const root = path.resolve(__dirname, "../..");
const run = path.resolve(root, process.env.IC_E2E_RUN || "e2e/.run");

function stack() {
  const file = path.join(run, "stack.json");
  if (!fs.existsSync(file)) throw new Error(`no stack is running from ${run}: start one with node e2e/stack/stack.js`);
  return JSON.parse(fs.readFileSync(file, "utf8"));
}
const shipped = (s) => JSON.parse(fs.readFileSync(path.join(s.run, "shipped.json"), "utf8"));

/// The ledger's business date, as the operations console reads it.
async function today(sys) {
  const r = await sys.must(sys.http("GET", sys.stack.api, "/v1/admin/reconciliation", { headers: { "x-dev-subject": "sub-st16", "x-dev-roles": "operations" } }), "read the business date");
  return r.business_date;
}

/// What a module's answer refused, if it did: the node's answer wraps the module's own, and either can refuse.
function refusal(reply) {
  if (reply && reply.result && reply.result.Err) return reply.result.Err;
  const inner = reply && reply.result && reply.result.Ok !== undefined ? reply.result.Ok : reply;
  if (inner && inner.result && inner.result.Err) return inner.result.Err;
  return inner && inner.refused ? inner.refused : null;
}

/// An application from a shipped template (`e2e/.run/shipped.json`, `applications`), for one party, with its IDs made
/// from the scenario's reference, and the changes a spec asks for: `covers` (sum insured by cover code, in cents),
/// `born`, `sex`, `smoker`, `class` (occupation class), `frequency`, `inception`, or a whole `application` to merge.
function application(s, spec, date) {
  const templates = shipped(s).applications;
  const a = JSON.parse(JSON.stringify(templates[spec.template] || {}));
  if (!templates[spec.template]) throw new Error(`no application template ${spec.template}; there are ${Object.keys(templates).join(", ")}`);
  if (!spec.ref || !spec.party) throw new Error("say ref (the scenario's reference, such as SCN202-1) and party (the customer's party)");
  const id = String(spec.ref).toLowerCase();
  Object.assign(a, { policy: id, number: String(spec.ref).toUpperCase(), group: `grp-${id}`, policyholder: spec.policyholder || spec.party, inception: spec.inception || date });
  for (const life of a.lives) if (life.kind.party) life.kind.party = spec.party;
  const factors = a.lives[0].factors;
  if (spec.born) factors.date_of_birth = spec.born;
  if (spec.sex) factors.sex = spec.sex;
  if (spec.smoker !== undefined) factors.smoker = spec.smoker;
  if (spec.class !== undefined) factors.occupation_class = spec.class === null ? null : String(spec.class);
  if (spec.frequency) a.frequency = spec.frequency;
  for (const [code, minor] of Object.entries(spec.covers || {})) {
    const cover = a.covers.find((c) => c.code === code);
    if (!cover) throw new Error(`the ${spec.template} template has no cover ${code}; it has ${a.covers.map((c) => c.code).join(", ")}`);
    if (minor === null) a.covers = a.covers.filter((c) => c !== cover); else cover.sum_insured_minor = minor;
  }
  return Object.assign(a, spec.application || {});
}

/// The core's quote for an application: the first period all in, and each cover's premium for a year.
async function quote(sys, a) {
  const priced = async (frequency) => {
    const r = await sys.api("POST", "/v1/quotes", { ...a, frequency });
    if (!r.ok) throw Object.assign(new Error(`the core would not quote ${a.product_code}: ${r.status} ${JSON.stringify(r.data)}`), { code: r.data && r.data.title });
    return r.data;
  };
  const sum = (m) => Object.values(m || {}).reduce((t, x) => t + x.minor, 0);
  const first = await priced(a.frequency);
  // A year's premium per cover is the yearly quote's; a product with no yearly premium (AIA Starter is monthly
  // only) is a year of the first period's.
  const periods = { weekly: 52, fortnightly: 26, monthly: 12, quarterly: 4, half_yearly: 2, yearly: 1 }[a.frequency] || 12;
  let yearly_by_cover;
  try {
    yearly_by_cover = Object.fromEntries(Object.entries((await priced("yearly")).by_cover).map(([code, m]) => [code, m.minor]));
  } catch (e) {
    if (e.code !== "frequency") throw e;
    yearly_by_cover = Object.fromEntries(Object.entries(first.by_cover).map(([code, m]) => [code, m.minor * periods]));
  }
  return { premium_minor: sum(first.by_cover) + sum(first.gst) + sum(first.levies) + first.policy_fee.minor, yearly_by_cover, quote: first };
}

/// The adviser named on an application: `{adviser, fap}`, the adviser's party and the provider they act under.
function adviser(spec) {
  if (!spec.adviser) return null;
  if (!spec.fap) throw new Error("an adviser acts under a provider: say fap as well (fap-d6 for adv-d1 and adv-d2, fap-kowhai for adv-d4 and adv-d5, fap-d9 for adv-d9a)");
  return { adviser: spec.adviser, fap: spec.fap };
}

const commands = {
  /// New business opens a proposal: `{ref, template, party, covers?, adviser?, fap?, option?, born?, sex?, smoker?, class?, frequency?}`.
  /// The proposal is `prop-<ref>`; the policy it issues is `<ref>`. Interim cover is written at once where the product has it.
  async propose(sys, spec) {
    const s = sys.stack;
    const date = await today(sys);
    const a = application(s, spec, date);
    const product = shipped(s).products[a.product_code];
    if (!product) throw new Error(`the seed knows no shipped version of ${a.product_code}`);
    const { premium_minor, yearly_by_cover, quote: q } = await quote(sys, a);
    const advised = adviser(spec);
    const proposal = `prop-${String(spec.ref).toLowerCase()}`;
    const open = {
      proposal, entry: { entry: "new_business" }, policy: a.policy, policy_number: a.number, group: a.group, policyholder: a.policyholder, product_code: a.product_code,
      frequency: a.frequency, channel: advised ? "adviser" : "direct", adviser: advised,
      quote: { product_hash: product.content_hash, rate_tables: [], premium_minor, quoted_on: date, cover_premiums_minor: yearly_by_cover },
      lives: a.lives.map((l) => ({ life: l.id, party: l.kind.party || a.policyholder, factors: l.factors })),
      covers: a.covers.map((c) => ({ code: c.code, cover_type: product.covers[c.code], life: c.life, sum_insured_minor: c.sum_insured_minor, ...(c.excess_option ? { option: c.excess_option } : {}) })),
      ...(advised && spec.option ? { commission_option: spec.option } : {}),
    };
    const reply = await sys.tell("underwriting", { key: `play-open-${proposal}`, caller: "new-business", at: new Date().toISOString(), today: date, command: { open_proposal: open } });
    const no = refusal(reply);
    if (no) throw new Error(`underwriting refused the proposal: ${JSON.stringify(no)}`);
    return { proposal, policy: a.policy, number: a.number, product: a.product_code, party: spec.party, premium_minor, quoted: q, business_date: date };
  },

  /// What underwriting holds on a proposal `{proposal}`: its status and, for each life, the stage, the cases and who
  /// holds them, the evidence asked for (the request IDs an evidence provider answers) and the offers.
  async proposal(sys, spec) {
    const p = await sys.ask("underwriting", { query: "proposal", proposal: spec.proposal });
    if (p && p.Err) throw new Error(`underwriting would not answer: ${JSON.stringify(p.Err)}`);
    const answer = p && p.Ok !== undefined ? p.Ok : p;
    if (!answer) throw new Error(`underwriting holds no proposal ${spec.proposal}`);
    if (spec.full) return answer;
    const lives = Object.values(answer.lives || {}).map((l) => ({
      life: l.life, party: l.party, stage: l.stage,
      cases: (l.cases || []).map((c) => ({ id: c.id, queue: c.queue, holder: c.holder ?? c.underwriter ?? null, status: c.status })),
      evidence: (l.evidence || []).map((e) => ({ request: e.id, kind: e.kind, status: e.status, deadline: e.deadline, provider_ref: e.provider_ref })),
      offers: (l.offers || []).length,
    }));
    return { proposal: answer.id, status: answer.status, policy: answer.policy, product: answer.product_code, interim: answer.interim || null, lives };
  },

  /// An evidence provider's answer to an order: `{request, outcome: "acknowledged", provider_ref}`,
  /// `{request, outcome: "received", report: "<text>" | report_file: "<path>", cost_minor}` or `{request, outcome: "unobtainable"}`.
  async evidence(sys, spec) {
    const s = sys.stack;
    if (!spec.request || !spec.outcome) throw new Error("say request (from play.js proposal) and outcome (acknowledged, received or unobtainable)");
    const body = { outcome: spec.outcome, request: spec.request };
    if (spec.outcome === "acknowledged") body.provider_ref = spec.provider_ref || `PRV-${crypto.randomBytes(3).toString("hex").toUpperCase()}`;
    if (spec.outcome === "received") {
      const report = spec.report_file ? fs.readFileSync(spec.report_file) : Buffer.from(spec.report || "");
      if (!report.length) throw new Error("a received report carries the report: give report (text) or report_file");
      body.report = report.toString("base64");
      body.cost_minor = spec.cost_minor ?? 0;
    }
    const bearer = await token(s, "ic-e2e-evidence-provider", s.audiences.intake);
    const r = await sys.http("POST", s.intake, "/v1/evidence", { headers: { authorization: `Bearer ${bearer}` }, body });
    if (!r.ok) throw new Error(`the underwriting intake refused the evidence: ${r.status} ${JSON.stringify(r.data)}`);
    return { filed: r.data, sent: { ...body, report: body.report ? `${body.report.length} base64 characters` : undefined } };
  },

  /// New business issues a policy straight from an application, with no underwriting (as the round 1 scenarios
  /// did): `{ref, template, party, covers?, inception?, ...}`; with `adviser` and `fap` it then advises commission
  /// (option `upfront` unless `option` says otherwise), as new business does once a policy is issued.
  async issue(sys, spec) {
    const date = await today(sys);
    const a = application(sys.stack, { ...spec, inception: spec.inception || date }, date);
    const r = await sys.api("POST", "/v1/policies", a);
    if (!r.ok || r.data.status !== "applied") throw new Error(`the core did not issue ${a.number}: ${r.status} ${JSON.stringify(r.data)}`);
    const out = { policy: a.policy, number: a.number, product: a.product_code, party: spec.party, partition: r.data.partition, inception: a.inception };
    if (spec.adviser) out.advised = await commands.advise(sys, { policy: a.policy, adviser: spec.adviser, fap: spec.fap, option: spec.option, application: a });
    return out;
  },

  /// New business tells commission who advised a policy: `{policy, adviser, fap, option?, replacement?, premiums? | application?}`.
  /// `premiums` is each cover's annual premium in cents by cover code; given the `application` instead, the core's
  /// yearly quote of it is used. Commission writes the layers into the book of the engagement between the two.
  async advise(sys, spec) {
    const advised = adviser(spec);
    if (!advised) throw new Error("say adviser and fap");
    let premiums = spec.premiums;
    if (!premiums) {
      if (!spec.application) throw new Error("say premiums (annual, in cents, by cover code) or give the application to quote");
      premiums = (await quote(sys, spec.application)).yearly_by_cover;
    }
    const advice = { adviser: advised.adviser, provider: advised.fap, option: spec.option || "upfront", replacement: !!spec.replacement, premiums };
    const reply = await sys.tell("distribution", { key: `play-advise-${spec.policy}`, actor: { module: "new-business" }, command: { advise_new_business: { policy: spec.policy, advice } } });
    const no = refusal(reply);
    if (no) throw new Error(`commission refused the advice: ${JSON.stringify(no)}`);
    return { policy: spec.policy, advice };
  },
};

async function main() {
  const [what, arg] = process.argv.slice(2);
  if (!commands[what]) {
    console.error(`usage: node e2e/stack/play.js <${Object.keys(commands).join("|")}> '<json>' (see the head of this file)`);
    process.exit(2);
  }
  const text = !arg ? "{}" : arg.startsWith("@") ? fs.readFileSync(arg.slice(1), "utf8") : arg;
  const sys = new System(stack());
  console.log(JSON.stringify(await commands[what](sys, JSON.parse(text)), null, 2));
}

module.exports = { commands, application, quote };

if (require.main === module) main().catch((e) => { console.error(String(e.message || e)); process.exit(1); });
