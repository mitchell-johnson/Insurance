# Insurance core

A core policy administration and ledger system for a life and health insurer, written in Rust and built to run on Azure.

It issues and services policies (retail term life, health, and group schemes), bills and collects premiums, matches incoming payments, runs the daily close, and keeps a double-entry ledger whose every entry is hash-chained, replicated and provable after the fact.

## How it is put together

| Process | What it does | Runs as |
| --- | --- | --- |
| `ic-node` | A ledger node. Holds a replica of every partition's Raft group (the log, the policy state, the ledger and the read models) and of every module's group. | StatefulSet of three on AKS |
| `ic-api` | The public HTTP API. A stateless gateway that routes each request to the partition that owns the policy. | Deployment |
| `ic-worker` | Runs the end-of-day close as a Temporal workflow, so a close survives any process dying part-way. | Deployment, plus a nightly CronJob |
| `ic-admin` | The operator's command: bootstrap, status, membership changes. | Job, or by hand |
| `ic-simulate` | A synthetic insurer that runs a year of business in seconds, for sizing and soak runs. | Developer tool |

Azure services do the work that should not be ours:

| Need | Azure service |
| --- | --- |
| Identity for every process, no secrets | Entra Workload ID, one managed identity per process |
| Caller identity on the API | Entra ID tokens |
| Tamper evidence: daily Merkle roots and sealed journal segments | Blob Storage with version-level immutability |
| Signing the daily root | Key Vault (or Managed HSM) |
| Events for downstream systems | Event Hubs, one hub partition per ledger partition |
| The cluster token | Key Vault, mounted through the Secrets Store CSI driver |
| Logs and metrics | Container Insights and Application Insights (JSON lines on stdout) |

[docs/architecture.md](docs/architecture.md) explains the design. [docs/operations.md](docs/operations.md) is the runbook. [docs/decisions.md](docs/decisions.md) lists what is deliberately not done yet and the decisions still open. [docs/modules.md](docs/modules.md) is the contract for the modules built beside the core.

## Try it

One process, everything in memory, no Azure:

```sh
IC_AUTH=dev IC_OPENING_DATE=2027-11-15 cargo run --bin ic-api
curl -s localhost:8080/readyz
```

The whole production arrangement on one machine (three ledger nodes, Temporal, two workers, the API), then broken on purpose:

```sh
deploy/compose/chaos.sh          # build, run the failure drill, tear down
KEEP=1 deploy/compose/chaos.sh   # leave the cluster up afterwards
```

## Build and test

Needs Rust (the toolchain is pinned in `rust-toolchain.toml`), `clang`, `protoc` and the protobuf well-known types (`protobuf-compiler` and `libprotobuf-dev` on Debian).

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 tools/check_test_docs.py
```

The end-of-day workflow test needs a Temporal server. It uses the one at `IC_TEST_TEMPORAL_ADDRESS` if set, otherwise it starts the dev server executable named by `IC_TEST_TEMPORAL_EXE`; with neither it fails and says what to set. `IC_SKIP_TEMPORAL_TESTS=1` leaves it out, and says so in the output.

The standards the suite is held to, enforced in CI (`.github/workflows/ci.yml`):

- Line coverage of at least 98% as `cargo llvm-cov --workspace` reports it (its own table, which counts a line again for each generic instantiation and enclosing closure), with the Temporal tests included. `tools/coverage_floor.py` lists the uncovered lines of each file.
- Every test carries a `Purpose:` and a `Why it matters:` line. `tools/check_test_docs.py` fails the build otherwise.
- Resilience is tested, not assumed: ledger node processes are killed mid-write, disks are destroyed, workers die mid-close, Azure goes away.

## Deploy to Azure

1. Create the platform: `az deployment group create -g <rg> -f infra/main.bicep -p infra/main.bicepparam`.
2. Build and push the image to the registry it created.
3. Fill in `deploy/k8s` from the deployment's outputs and `kubectl apply -k deploy/k8s`.

[docs/operations.md](docs/operations.md) has each step in full.

## Repository layout

```
crates/money      exact decimal money and rounding
crates/ledger     double-entry ledger, hash chain, Merkle roots
crates/log        the ordered command log of one partition
crates/product    product definitions (Starlark), rating, schedules
crates/core       the policy state machine: issue, bill, collect, lapse, transfer, close
crates/readmodel  policy directory, life index, billing view, audience scope
crates/cashmatch  matching incoming payments to policies
crates/raft       ledger nodes: Raft groups, storage, transport, the node server
crates/workflows  the end-of-day close and its Temporal workflow
crates/azure      Entra, Blob Storage, Key Vault and Event Hubs adapters
crates/api        the HTTP API, the gateway, ic-admin
crates/sim        in-process cluster and the synthetic insurer
crates/reinsurance treaties, per-life accumulation, cessions, capacity, recoveries, reinsurer accounts
crates/channels   the customer app API, adviser and employer portals, staff and operations consoles, the status page (ic-portal)
crates/underwriting proposals, disclosure, rules, case work and authority, offers, issue, the clinical store
treaties/         the shipped reinsurance treaties and reinsurer rate tables
products/         the shipped products, definitions and rate tables
infra/            the Azure platform (Bicep)
deploy/compose    the cluster on one machine, and the failure drill
deploy/k8s        the cluster on AKS
docs/             architecture, operations, decisions, the persona test pack
```
