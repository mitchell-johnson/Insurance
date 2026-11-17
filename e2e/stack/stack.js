// Starts the whole system on one machine as real processes, wired together
// as they are in production, and seeds it with the persona cast:
//
//   three ledger nodes (every partition and every module, replicated with
//   Raft), the API, the portal, the group service, the customer master with
//   its PostgreSQL, the documents service, the underwriting intake, and the
//   drivers that carry work between modules (underwriting, reinsurance). The
//   unit pricing driver and the bank pump are not run: tests play their part.
//   Azure is the stand-in in `azure.js`.
//
//   node e2e/stack/stack.js            start, seed, then wait until stopped
//   node e2e/stack/stack.js --no-seed  start only
//
// The binaries come from `IC_BIN` (default `target/debug`): build them first
// with `cargo build --bins`. Everything it writes goes under `IC_E2E_RUN`
// (default `e2e/.run`): data, logs, and `stack.json`, which tells the tests
// where each service listens. Nothing here is a mock of our own code.
"use strict";
const fs = require("fs");
const path = require("path");
const { spawn, spawnSync } = require("child_process");
const azure = require("./azure");

const root = path.resolve(__dirname, "../..");
const bin = path.resolve(root, process.env.IC_BIN || "target/debug");
const run = path.resolve(root, process.env.IC_E2E_RUN || "e2e/.run");
const P = Number(process.env.IC_E2E_PORT || 17000);
const ports = { nodes: [P + 1, P + 2, P + 3], api: P + 80, portal: P + 90, azure: P + 100, group: P + 110, customers: P + 120, documents: P + 130, intake: P + 140, postgres: P + 432 };
const TENANT = "e2e-tenant";
const TOKEN = "e2e-only-cluster-token-0123456789abcdef";
const PARTITIONS = 4;
const OPENING = process.env.IC_E2E_OPENING_DATE || "2027-11-15";
const local = (port) => `127.0.0.1:${port}`;
const url = (port) => `http://${local(port)}`;
const NODES = ports.nodes.map(local).join(",");

// The app registrations: each service's audience, and the roles each calling
// workload has been assigned on it. This is the role assignment a deployment
// makes in Entra (docs/channels.md, docs/group.md, docs/customers.md,
// docs/documents.md, docs/underwriting.md); a role missing here fails the
// same way it would there.
const AUD = { group: "api://ic-group", customers: "api://ic-customers", documents: "api://ic-documents", intake: "api://ic-underwriting-intake" };
const roles = {
  // The portal's roles on the customer master are those `docs/operations.md` assigns it: the customer page, staff
  // work, the privacy officer's, and operations' (the search index and the record chain check).
  "ic-portal": { [AUD.group]: ["employer-portal", "group-admin", "member-portal"], [AUD.customers]: ["portal", "staff", "privacy", "operations"], [AUD.documents]: ["portal", "staff"], [AUD.intake]: ["new-business", "underwriting-console"] },
  "ic-group": { [AUD.customers]: ["group-admin"] },
  "ic-documents": { [AUD.customers]: ["documents"] },
  "ic-payments": { [AUD.customers]: ["payments"] },
  // The suite's stand-in for new business opening a customer's record, as it does before a policy is issued.
  "ic-e2e-new-business": { [AUD.customers]: ["new-business"] },
  // The suite's stand-in for an evidence provider's adapter (a GP report, a blood test), which reports an order
  // acknowledged, received with its cost, or unobtainable to the underwriting intake (`e2e/stack/play.js evidence`).
  "ic-e2e-evidence-provider": { [AUD.intake]: ["evidence"] },
  "e2e-seed": { [AUD.group]: ["group-admin", "employer-portal"], [AUD.customers]: ["portal", "group-admin", "identity", "staff", "new-business", "claims"], [AUD.documents]: ["portal", "staff"], [AUD.intake]: ["new-business", "underwriting-console"] },
};

const children = [];
let stopping = false;
function start(name, exe, env, args = []) {
  fs.mkdirSync(path.join(run, "logs"), { recursive: true });
  const out = fs.openSync(path.join(run, "logs", `${name}.log`), "a");
  const child = spawn(path.join(bin, exe), args, { env: { PATH: process.env.PATH, RUST_LOG: process.env.RUST_LOG || "info", ...env }, stdio: ["ignore", out, out] });
  child.on("exit", (code, signal) => { if (!stopping) console.error(`${name} exited (${code ?? signal}); see ${path.join(run, "logs", name + ".log")}`); });
  children.push({ name, child });
  return child;
}
function once(exe, args, env) {
  const r = spawnSync(path.join(bin, exe), args, { env: { PATH: process.env.PATH, ...env }, encoding: "utf8" });
  if (r.status !== 0) throw new Error(`${exe} ${args.join(" ")} failed: ${r.stderr || r.stdout}`);
  return r.stdout;
}
async function until(what, check, seconds = 120) {
  const end = Date.now() + seconds * 1000;
  let last = "";
  while (Date.now() < end) {
    try { if (await check()) return; } catch (e) { last = String(e.message || e); }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`timed out waiting for ${what}${last ? ": " + last : ""}`);
}
const ok = async (address) => (await fetch(address)).ok;

// Each workload's identity, as the AKS workload identity webhook injects it.
function identity(client) {
  const file = path.join(run, "identity", client);
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `federated-token-of-${client}`);
  return { AZURE_FEDERATED_TOKEN_FILE: file, AZURE_TENANT_ID: TENANT, AZURE_CLIENT_ID: client, AZURE_AUTHORITY_HOST: url(ports.azure) };
}
const cluster = { IC_NODES: NODES, IC_CLUSTER_NODES: NODES, IC_CLUSTER_TOKEN: TOKEN };

// PostgreSQL for the customer master. `IC_E2E_POSTGRES_URL` names a server that is already running (the workflow starts
// one in a container, as `ci.yml` does); otherwise a throwaway server is made under the run directory from the
// PostgreSQL installed on this machine (`IC_E2E_PG_BIN`, or the newest under /usr/lib/postgresql, or the PATH).
async function postgres() {
  if (process.env.IC_E2E_POSTGRES_URL) return process.env.IC_E2E_POSTGRES_URL;
  const installed = "/usr/lib/postgresql";
  const versions = fs.existsSync(installed) ? fs.readdirSync(installed).sort((a, b) => Number(b) - Number(a)) : [];
  const pg = process.env.IC_E2E_PG_BIN || (versions.length ? path.join(installed, versions[0], "bin") : "");
  const tool = (name) => (pg ? path.join(pg, name) : name);
  // PostgreSQL refuses to run as root: in a container that is root, it runs as the postgres user, in a directory of its own.
  let as = {};
  let data = path.join(run, "postgres");
  if (process.getuid && process.getuid() === 0) {
    const id = (flag) => Number(spawnSync("id", [flag, "postgres"], { encoding: "utf8" }).stdout.trim());
    as = { uid: id("-u"), gid: id("-g") };
    if (!as.uid) throw new Error("this machine runs as root and has no postgres user; set IC_E2E_POSTGRES_URL to a running server");
    data = fs.mkdtempSync(path.join(require("os").tmpdir(), "ic-e2e-postgres-"));
    fs.chownSync(data, as.uid, as.gid);
    fs.writeFileSync(path.join(run, "postgres-data"), data);
  }
  const made = spawnSync(tool("initdb"), ["-D", data, "-U", "postgres", "--auth=trust", "-E", "UTF8"], { ...as, encoding: "utf8" });
  if (made.status !== 0) throw new Error(`initdb failed (is PostgreSQL installed? set IC_E2E_PG_BIN or IC_E2E_POSTGRES_URL): ${made.stderr || made.error}`);
  fs.mkdirSync(path.join(run, "logs"), { recursive: true });
  const out = fs.openSync(path.join(run, "logs", "postgres.log"), "a");
  const child = spawn(tool("postgres"), ["-D", data, "-c", "listen_addresses=127.0.0.1", "-c", `port=${ports.postgres}`, "-c", `unix_socket_directories=${data}`, "-c", "fsync=off", "-c", "max_connections=100"], { ...as, stdio: ["ignore", out, out] });
  child.on("exit", (code, signal) => { if (!stopping) console.error(`postgres exited (${code ?? signal}); see ${path.join(run, "logs", "postgres.log")}`); });
  children.push({ name: "postgres", child });
  await until("PostgreSQL", () => spawnSync(tool("pg_isready"), ["-h", "127.0.0.1", "-p", String(ports.postgres)]).status === 0, 60);
  return `host=127.0.0.1 port=${ports.postgres} user=postgres dbname=postgres`;
}

/// The PostgreSQL directory a root-run stack keeps outside the run directory, removed with the run it belonged to.
function forget() {
  const note = path.join(run, "postgres-data");
  if (!fs.existsSync(note)) return;
  const data = fs.readFileSync(note, "utf8").trim();
  if (path.basename(data).startsWith("ic-e2e-postgres-")) fs.rmSync(data, { recursive: true, force: true });
}

async function up() {
  forget();
  if (fs.existsSync(run)) fs.rmSync(run, { recursive: true, force: true });
  fs.mkdirSync(run, { recursive: true });
  const az = await azure.start({ port: ports.azure, roles });

  // The ledger: three nodes, two regions (two in the home region), every module hosted.
  const peers = ports.nodes.map((p, i) => `${i + 1}=${local(p)}`).join(",");
  ports.nodes.forEach((p, i) => start(`node-${i + 1}`, "ic-node", {
    ...cluster, IC_NODE_ID: String(i + 1), IC_NODE_PEERS: peers, IC_NODE_LISTEN: local(p), IC_NODE_DATA_DIR: path.join(run, "data", `node-${i + 1}`),
    IC_PARTITIONS: String(PARTITIONS), IC_COLD_DIR: path.join(run, "cold"), IC_EVENT_SINK: "discard", IC_NODE_BOOTSTRAP: i === 0 ? "true" : "false",
  }));
  const admin = { ...cluster, IC_LEDGER_PATIENCE_SECS: "120" };
  await until("the ledger nodes", async () => { once("ic-admin", ["init"], admin); return true; }, 60);
  once("ic-admin", ["open", OPENING], admin);
  once("ic-admin", ["activate-shipped"], admin);

  // The customer master: a service of its own with a PostgreSQL database, because personal data must be erasable.
  const database = await postgres();
  start("customers", "ic-customers", {
    ...cluster, ...identity("ic-customers"), IC_CUSTOMERS_DATABASE_URL: database, IC_CUSTOMERS_KEY_VAULT: az.base, IC_CUSTOMERS_INDEX_KEY: "e2e-only-index-key-0123456789abcdefghij",
    IC_CUSTOMERS_TENANT: TENANT, IC_CUSTOMERS_AUDIENCE: AUD.customers, IC_CUSTOMERS_AUTHORITY: az.base, IC_EVENT_HUBS_NAMESPACE: az.base,
    IC_CUSTOMERS_REGISTER_ENDPOINT: az.base, IC_CUSTOMERS_LISTEN: local(ports.customers), IC_CUSTOMERS_CYCLE_SECONDS: "1",
  });
  // Group scheme administration: member files, billing and the employer's side of a scheme.
  start("group", "ic-group", {
    ...cluster, ...identity("ic-group"), IC_GROUP_QUARANTINE_ENDPOINT: az.base, IC_GROUP_EVENT_HUBS_NAMESPACE: az.base, IC_GROUP_CUSTOMER_MASTER: url(ports.customers),
    IC_GROUP_CUSTOMER_MASTER_SCOPE: `${AUD.customers}/.default`, IC_GROUP_FINGERPRINT_SALT: "e2e-only-fingerprint-salt-0123456789", IC_GROUP_AUDIENCE: AUD.group,
    IC_GROUP_LISTEN: local(ports.group), IC_GROUP_CYCLE_SECONDS: "1",
  });
  start("api", "ic-api", { ...cluster, IC_PARTITIONS: String(PARTITIONS), IC_AUTH: "dev", IC_LISTEN: local(ports.api) });
  start("portal", "ic-portal", {
    ...cluster, ...identity("ic-portal"), IC_AUTH: "dev", IC_PORTAL_LISTEN: local(ports.portal), IC_PARTITIONS: String(PARTITIONS),
    IC_REGIONS: `auckland=${local(ports.nodes[0])},${local(ports.nodes[1])};sydney=${local(ports.nodes[2])}`, IC_HOME_REGION: "auckland",
    IC_PORTAL_HEARTBEAT_SECS: "5", IC_PORTAL_FORWARD_SECS: "2",
    // The portal takes plain http only from a host on this machine, which it knows by name.
    IC_GROUP_URL: `http://localhost:${ports.group}`, IC_GROUP_SCOPE: `${AUD.group}/.default`, IC_CUSTOMERS_URL: `http://localhost:${ports.customers}`, IC_CUSTOMERS_SCOPE: `${AUD.customers}/.default`,
    IC_DOCUMENTS_URL: `http://localhost:${ports.documents}`, IC_DOCUMENTS_SCOPE: `${AUD.documents}/.default`,
    IC_UNDERWRITING_INTAKE_URL: `http://localhost:${ports.intake}`, IC_UNDERWRITING_INTAKE_SCOPE: `${AUD.intake}/.default`,
  });
  // The documents service: letters, notices and the vault, composed by the built-in renderer.
  start("documents", "ic-documents", {
    ...cluster, ...identity("ic-documents"), IC_DOCUMENTS_WEBHOOK_KEY: "e2e-only-webhook-key-0123456789abcdefghij", IC_DOCUMENTS_VAULT_ENDPOINT: az.base,
    IC_DOCUMENTS_KEY_VAULT: az.base, IC_COMMUNICATION_ENDPOINT: az.base, IC_DOCUMENTS_EMAIL_FROM: "letters@insurer.test", IC_DOCUMENTS_SMS_FROM: "+6421000000",
    IC_CUSTOMER_MASTER_URL: `http://localhost:${ports.customers}`, IC_CUSTOMER_MASTER_SCOPE: `${AUD.customers}/.default`, IC_EVENT_HUBS_NAMESPACE: az.base,
    IC_DOCUMENTS_LISTEN: local(ports.documents), IC_ENTRA_TENANT: TENANT, IC_DOCUMENTS_AUDIENCE: AUD.documents, IC_ENTRA_AUTHORITY: az.base, IC_DOCUMENTS_CYCLE_SECONDS: "1",
  });
  // New business and underwriting: the intake that takes proposals and clinical evidence, and the driver that carries
  // underwriting's decisions to the core and its notices to the other modules.
  start("intake", "ic-underwriting-intake", {
    ...cluster, ...identity("ic-underwriting-intake"), IC_UNDERWRITING_INTAKE_LISTEN: local(ports.intake), IC_ENTRA_AUTHORITY: az.base, IC_ENTRA_TENANT_ID: TENANT,
    IC_UNDERWRITING_INTAKE_AUDIENCE: AUD.intake, IC_CLINICAL_BLOB_ENDPOINT: az.base, IC_CLINICAL_KEY_VAULT_URL: az.base, IC_CLINICAL_WRAPPING_KEY: "clinical-kek",
  });
  start("underwriting", "ic-underwriting", { ...cluster, ...identity("ic-underwriting"), IC_EVENT_HUBS_ENDPOINT: az.base, IC_UNDERWRITING_CYCLE_MS: "1000" });
  // The reinsurance driver: cessions and notices to the core, records filed write-once.
  start("reinsurance", "ic-reinsurance", { ...cluster, ...identity("ic-reinsurance"), IC_BLOB_ENDPOINT: az.base, IC_EVENT_HUBS_ENDPOINT: az.base, IC_REINSURANCE_CYCLE_MS: "1000" });
  await until("the API", () => ok(url(ports.api) + "/readyz"));
  await until("the customer master", () => ok(url(ports.customers) + "/readyz"));
  await until("the group service", () => ok(url(ports.group) + "/readyz"));
  await until("the portal", () => ok(url(ports.portal) + "/readyz"));
  await until("the documents service", () => ok(url(ports.documents) + "/healthz"));
  await until("the underwriting intake", () => ok(url(ports.intake) + "/healthz"));

  const stack = { portal: url(ports.portal), api: url(ports.api), group: url(ports.group), customers: url(ports.customers), documents: url(ports.documents), intake: url(ports.intake), azure: az.base, nodes: ports.nodes.map(url), token: TOKEN, tenant: TENANT, partitions: PARTITIONS, opening: OPENING, audiences: AUD, run };
  fs.writeFileSync(path.join(run, "stack.json"), JSON.stringify(stack, null, 2));
  return { stack, az };
}

async function down() {
  stopping = true;
  for (const { child } of children.reverse()) child.kill("SIGTERM");
  await new Promise((r) => setTimeout(r, 1500));
  for (const { child } of children) if (child.exitCode === null) child.kill("SIGKILL");
  forget();
}

module.exports = { up, down, ports, roles, AUD, TENANT, TOKEN, url };

if (require.main === module) {
  process.on("SIGINT", () => down().then(() => process.exit(0)));
  process.on("SIGTERM", () => down().then(() => process.exit(0)));
  up().then(async ({ stack }) => {
    if (!process.argv.includes("--no-seed")) await require("./seed").seed(stack);
    console.log(`ready ${stack.portal}`);
  }).catch(async (e) => { console.error(String(e.stack || e)); await down(); process.exit(1); });
}
