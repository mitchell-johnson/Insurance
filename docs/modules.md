# Building a module beside the core

The core (ledger, policy model, servicing, billing, the Raft nodes, the daily close, the Azure adapters) is one part of the system. Claims, reinsurance, group administration, commission, payments, underwriting, documents, finance and the portal services are modules built beside it. This page is the contract between them.

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

- **State that belongs to one policy on one partition** (a claim, for example) lives in that partition's replicated state, attached through the core's module port (`CoreState::apply_module`). It is in the partition's snapshot and state hash, counts against the apply budget, and commits or fails together with the core command it issues.
- **State that spans partitions** (per-life reinsurance accumulation, an adviser's commission account, the finance ledger) lives in the module's own replicated group on the ledger nodes, with the same log, snapshots and failover as a partition, fed from every partition's event stream. The generic host for this is in `ic-raft`. Until a module is attached to it, run the state machine over `ic_log::Replica` with a `FileLog`.

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
