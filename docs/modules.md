# Building a module beside the core

The core (ledger, policy model, servicing, billing, the Raft nodes, the daily close, the Azure adapters) is one part of the system. Claims, reinsurance, underwriting, group administration, commission, payments, collections, finance, unit pricing, documents, complaints, the portal services (channels) and wellness are modules built beside it. Claims lives inside each partition; the other twelve each run as their own replicated group on the ledger nodes, listed in `ic_node::modules()`. The customer master (`crates/customers`) is not a module: it is a separate service with its own PostgreSQL, because personal data must be erasable, and modules reach it over HTTP by party ID. This page is the contract between them.

## Layout

- One crate per module at `crates/<name>`, package name `ic-<name>`. Every directory under `crates/` is a workspace member; adding a module does not touch the root `Cargo.toml`.
- Depend on sibling crates by path in your own `Cargo.toml` (`ic-core = { path = "../core" }`) and take third-party versions from `[workspace.dependencies]` where they are already listed. Add a new third-party dependency to the workspace table only when two crates need it.
- If `Cargo.lock` conflicts when you merge `main`, take `main`'s copy and run `cargo build` to add your entries back. Never resolve it by hand.
- Tests live in `crates/<name>/tests/`. Every test carries a doc comment with `Purpose:` and `Why it matters:` lines; `tools/check_test_docs.py` fails the build otherwise.

## What a module is

A module is a deterministic state machine: it implements `ic_log::StateMachine`, changes state only by applying a logged command, and never reads a clock, randomness or the network while applying. Everything that talks to the outside (HTTP, Azure, a bank) is a driver around it that proposes commands and acts on its outbox, and can be retried.

A module reads the core in two ways and writes to it in one:

- **Events.** Each partition publishes a gapless, ordered stream of `ic_core::EventRecord` (`partition`, `seq`). A consumer keeps a checkpoint per partition, de-duplicates on `id`, and asks for missing events when it sees a gap. Events never carry personal or health data; a module that needs a person asks the customer master by party ID.
- **Queries.** Read models (`ic-readmodel`) and the node query endpoint answer questions about current state. Do not read another module's state directly.
- **Commands.** A module changes a policy or posts money only by sending the core a typed `ic_core::Command` through `ic_raft::ClusterClient`, as a named caller, with a stable idempotency key derived from its own state (not a random one), so a retry after a timeout applies once.

## Where a module's state lives

- **State that belongs to one policy on one partition** (a claim, for example) lives in that partition's replicated state, as the claims module does: the core holds the module's state (`CoreState::claims`), takes its commands as `Command::Claims`, and gives it a port (`ic_claims::Core`) for the cover check, postings and published facts. See `docs/claims.md`. It is in the partition's snapshot and state hash, counts against the apply budget, and commits or fails together with the core command it issues.
- **State that spans partitions** (per-life reinsurance accumulation, an adviser's commission account, the finance ledger) lives in the module's own replicated group on the ledger nodes, with the same log, snapshots, membership changes and failover as a partition.

## The module host

`ic_raft::module` hosts a module's group. The host knows nothing about any module; a module hands it a state machine and a name.

```rust
use ic_raft::{hosted, ModuleSpec, ModuleState};

fn reinsurance() -> Box<dyn ModuleState> {
    // Any ic_log::StateMachine that is Serialize + DeserializeOwned, plus a
    // function that answers questions from it.
    hosted(Accumulation::default(), answer)
}

let modules = vec![ModuleSpec::new("reinsurance", reinsurance)?];
ic_raft::server::start_with(settings, modules).await?;
```

- A command is the JSON of the state machine's `Command`; the reply is the JSON of its `Output`. Send it with `ClusterClient::propose_module(name, &json)` (or `propose_module_until`), which finds the group's leader and follows redirects. `POST /v1/modules/{name}/commands` is the route underneath.
- Delivery is at least once. Put an idempotency key in the command and answer a repeat with the original result, as the core does.
- A command the module cannot decode is refused (`ModuleReply.result` is `Err`) and changes nothing. A module that wants to refuse a well-formed command returns its own error in its `Output`.
- `ClusterClient::query_module(name, &json)` asks the leader, after it has confirmed with a majority that it still leads. Queries never change state.
- The state is saved as one JSON value for snapshots and hashed for `GET /v1/modules`, where every member reports its state hash. Members that applied the same entries must report the same hash, so keep maps as `BTreeMap`, never `HashMap`, and keep everything a replica holds inside the saved state.
- The group lives in `<data dir>/module-<name>` on each node. The name is permanent. Every node of a cluster hosts the same modules; `PUT /v1/modules/{name}/members` replaces a failed member exactly as for a partition.
- A node that is given a directory holding another module's snapshot does not start.

Until a module is attached to the host, run the state machine over `ic_log::Replica` with a `FileLog`; moving it across is the `hosted(...)` line above.

### Getting hosted on the deployed node

The node that is deployed is the `ic-node` binary in `crates/node`. It depends on every module crate and lists their specs in one function, `ic_node::modules()`. To be hosted:

1. Expose `pub fn module_spec() -> Result<ic_raft::ModuleSpec, String>` from your crate.
2. Add your crate to `crates/node/Cargo.toml` and one line to `modules()` in `crates/node/src/lib.rs`.

That is the only place modules are attached. `crates/node/tests/node.rs` starts a node with the whole list and fails if a group does not form or two modules share a name. Do not ship a node binary of your own: two binaries named alike collide in the build, and a module hosted by a private binary is not in the cluster anyone deploys. `ic-core-node` in `ic-raft` is a node with no modules, kept for the Raft crate's own process tests.

### Reading the core's events

A module that reads events is declared with `hosted_reading(state, query, react)` and `ModuleSpec::new(name, make)?.reading_events()`. `react` is a pure function from an `ic_core::EventRecord` to the module's own commands.

- The node leading the module's group fetches each partition's events after the group's checkpoint and proposes them through the group's log. Every member is shown each event once, in the partition's sequence. The checkpoint per partition is part of the replicated state and of the state hash, so a new leader carries on where the last one stopped, and a run offered twice is taken in once.
- Each partition keeps its events until they are on the stream and every registered reader has acknowledged them. A module hosted from the first start misses nothing. A module added to a cluster that is already running starts at the oldest event the partition still holds; what came before is on Event Hubs and in the cold tier, and loading a new module from there is not built.
- `react` sees events only. To change a policy the module's driver sends the core a command, as above.
- A reader outside the cluster uses the same routes: `ClusterClient::acknowledge_events(partition, consumer, 0)` registers it, `events(partition, consumer, after, max)` reads from its checkpoint, `acknowledge_events` lets the partition release what it has, and `unsubscribe` ends it. A reader that stops acknowledging holds events in every member's state: `backlog.events` in `GET /v1/status` is the figure to watch.

### Posting to the core

A module changes the ledger only by sending the core a command. It does not
call the core while applying its own command, because that would not be
deterministic. It writes the command to an outbox in its own replicated
state, in the same step as whatever caused it, and the host carries it:

```rust
ModuleSpec::new(MODULE, make)?.posting_to_core(Outbox { pending, read, outcome })
```

- `pending(limit)` is the question that lists the oldest unsent items.
- `read(answer)` turns the module's answer into `CorePosting { id, route, envelope }`,
  oldest first. `route` is `Route::Partition(p)` when the module knows the
  partition and `Route::Policy(id)` when the host should find the partition
  that holds the policy.
- `outcome(posting, outcome)` is the module command that records what became
  of the item: `Applied { partition, log_index, duplicate }`, `Refused(reject)`
  with the core's own reason, or `Unroutable`.

The node that leads the module's group carries the outbox on every
housekeeping pass. The rules a module must keep:

- Remove an item only on hearing its outcome. A node lost between the core
  applying the command and the module hearing of it means the item is offered
  again, so the envelope's idempotency key must be the item's own and never
  change; the core then answers the second offer with the first result.
- The outcome command carries its own idempotency key and may arrive twice.
- A refusal is final. Offering the same command again changes nothing, so
  record it and raise whatever a person needs to see.
- Items are carried oldest first and one that cannot be offered holds back
  those behind it. If the module answers an outcome with a refusal of its own
  the host cannot tell, and carries on with the next item.

## Changing the core

The core's commands and events are a published interface. Any thread may extend it; nobody may break it.

- **Additive only.** New `Command` and `Event` variants, new fields with `#[serde(default)]`. No renames, no removed fields, no change to what an existing command does or an existing event means. If you need one of those, ask the core thread.
- Put a module's core-side handlers in one file of their own (`crates/core/src/<module>.rs`) with thin arms in `command.rs`, `event.rs` and `machine.rs`.
- Every new `Command` needs its callers in `allowed_callers`, its name in `name`, and the ownership check in `crates/sim/tests/integrity.rs`.
- Money moves only through balanced journal entries. New accounts are declared in `accounts.rs`, and `recon.rs` must still reconcile to zero breaks after your change. A reversal is a new entry carrying the original value date (`EntryDraft.value_date`) and the entry it reverses (`detail["reverses"]`); nothing posted is ever edited.
- Validate before you write. A rejected command leaves no trace in state.
- Active configuration is immutable: a product, rate table or definition version cannot be re-activated with other content (`config_conflict`). Change by new version.
- Anything a replica holds must be in the state hash and the snapshot, or replicas can differ without anyone noticing.

## Before you push

Run what CI runs:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 tools/check_test_docs.py
IC_SKIP_TEMPORAL_TESTS=1 cargo test --workspace --locked
```

CI also holds line coverage at 98% as `cargo llvm-cov --workspace` reports it, and prints the uncovered lines of each file. Code pushed without tests lowers the figure for everyone.

Work merges straight to `main`. Merge `origin/main` into your branch before every push, and never force-push `main`. Keep commits small so merges stay small.
