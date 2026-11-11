# Architecture

## The shape of it

Everything the insurer does to a policy is a **command**: issue, apply a receipt, change scheme membership, close the day. A command goes into an ordered, replicated log, and a deterministic state machine applies the log. The state machine holds the policies and a double-entry ledger, and it is the only thing that ever writes either. Given the same log, every replica reaches the same state and the same ledger, byte for byte, and each replica proves it with a state hash.

That one idea gives the properties an insurer's core needs:

- **Nothing is lost.** A command is acknowledged only after a majority of replicas have it on disk.
- **Nothing happens twice.** Every command carries an idempotency key. A retry of a command that was applied returns the original result.
- **Everything is explainable.** The ledger is append-only and hash-chained. Policy state can be rebuilt by replaying the log.
- **Tampering shows.** Each day's ledger heads are folded into a Merkle root, signed with a Key Vault key and written to immutable storage.

## Partitions

Policies are grouped (a household, an employer's scheme), and a group lives on one of 64 partitions, chosen by a hash of the group's ID that never changes. Each partition is its own Raft group with its own log, ledger and business date. Work on one partition never waits for another, and one very large scheme cannot stall the rest: no command may do more than a bounded amount of work, so a 50,000-member scheme is billed in chunks.

A policy can move between groups, and so between partitions. The move is a two-step transfer with money held in an in-transit account between the steps, delivered by messages that are retried until acknowledged. Reconciliation checks every day that nothing is left in transit.

## Ledger nodes (`ic-node`)

A node holds a replica of every partition. Three nodes, one per availability zone, tolerate the loss of any one.

- **Raft** (openraft) orders the log. Each node's log is an append-only journal, `fsync`ed before an entry is acknowledged. Members ask before they campaign (pre-vote), so a member returning from a network fault does not force an election on a healthy group.
- **Snapshots** bound the log. The state machine is snapshotted as the log grows, and a new or lagging node is caught up from a snapshot.
- **Identity.** A node's number is bound to an identity created on its disk the first time it starts. A node that comes back with an empty disk under its old number is refused by its peers, because it might vote again in a term it already voted in. It is replaced under a new number instead.
- **Housekeeping.** The leader of each partition seals finished stretches of the journal to immutable Blob Storage, publishes events to Event Hubs and delivers cross-partition messages. Each is at-least-once, recorded in the log when done, and resumed by the next leader if the leader dies.
- **Read models.** Each node keeps its partitions' read models (the policy directory, the life index used for sum-at-risk, the billing view) inside the replicated state, so a query answered by a node is consistent with the ledger it sits beside.
- **Authentication.** Every caller of a node presents the cluster token. The token lives in Key Vault.

## The API (`ic-api`)

A stateless gateway. It authenticates the caller's Entra ID token, checks the caller's role against the command, finds the partition (by group, by policy, or by asking every partition for a policy number), and proposes the command to that partition's leader, retrying across leader changes with the same idempotency key. Reads are linearizable: they are answered by the leader after it confirms it still leads.

When the ledger cannot be reached the API answers `503` with `Retry-After`, and a retry with the same idempotency key is always safe.

The same binary, started without `IC_NODES`, is the "insurance in a box": every partition in one process, for development and demonstrations. It is not the production arrangement.

## The daily close (`ic-worker`)

At midnight in New Zealand each business day is closed: the cutoff, billing, arrears and lapses, earning, reconciliation, then the signed Merkle root. The close is a Temporal workflow whose steps are each safe to repeat any number of times, by any worker. Temporal records each finished step, so when a worker dies another continues from the step after the last recorded one. A day whose books do not reconcile fails the close, and the next day is not opened over it.

The business date moves only by the close. Nothing in the core reads the wall clock.

## Money and products

Money is exact decimal arithmetic with explicit rounding. Rounding differences are posted, never dropped. Products, rate tables and policy definitions are data: Starlark files under `products/`, activated through the log like any other command, so the product version a policy was sold under is on record and replayable. An active version is immutable: the ledger accepts the same content again, and refuses other content under a product, rate table or definition version already in use, or another value for an index rate already in effect (`config_conflict`). A product changes by activating a new version.

## Events notified late

An insured event can be notified weeks after it happened, and by then the policy has carried on: periods were billed, an anniversary applied indexation, a cover may have expired by age. `TerminateOnClaim` ends the policy as at the day of the event. It first unwinds everything effective after that day, then earns premium up to and including the day, and returns the premium for every later day to policy suspense.

`UnwindAfter` is the same reversal on its own, for a claim or for a remediation case correcting the insurer's error. Nothing is removed or rewritten:

- Each period billed after the date is reversed by a new entry with the period's own value date, naming the entry it reverses (`detail["reverses"]`) and the claim or case (`detail["reference"]`). The hash chain extends over the reversals and every published root still verifies.
- A layer that started after the date is ended on its start date, so it existed for no days and its history shows it was created and unwound. A cover that expired after the date is in force again.
- The policy is then held as at the date: end-of-day earns what is still billed and does nothing else, so the anniversary just reversed is not applied again that night. A claim ends the hold by terminating the policy. A remediation case ends it with `ResumeAfterUnwind`, and end-of-day replays the anniversaries and billing from the date on the terms then in force.

By default an indexation step applied after an event nobody yet knew of is treated as never having started, and `PolicyTerminated` reports the sum insured as at the event. A product whose wording pays the indexed amount says so (`indexation(..., pays_step_applied_after_event = True)`), and the step then stands.

Refunds go back to the account the money came from. Where the payer is the person the claim is for (`payer_deceased`), the core cancels the payment authority (`PaymentAuthorityCancelled`) and refuses any refund that is not paid with that claim (`RefundSuspense.with_claim`).

## Choices made on a defective notice

A choice made in reliance on a wrong mandatory notice is not a valid choice. The documents module finds the defect, sends a corrected notice, and once that has proof of delivery it opens a reopen window on the policy (`OpenReopenWindow`, idempotent by reference). Inside the window the policyholder may make the choice again with `ReopenChoice`, and it applies as if made correctly the first time: no fee, no underwriting, no waiting period, nothing asked of the policyholder first.

- **A decline of indexation set aside.** If the latest anniversary passed with indexation declined, the step it would have added is added as a layer that starts on the anniversary. The periods billed since are reversed by new entries with their own value dates and billed again at the premium that includes the step. The difference is due on the last day of the window, so making the choice again never puts the policy into arrears. The decline is removed from the policy's record of declined anniversaries. If the anniversary has not come yet, the choice is recorded and the anniversary indexes the cover in the ordinary way.
- **A cancellation set aside.** The cancellation's own entry is reversed at its own value date, and the covers, receivables and standing are put back as they were, so cover is unbroken. If the refund has already been paid out, the policy is in force all the same and the refund is owed again as a receivable due at the end of the window.
- **The notice clock.** `RecordNoticeDelivery` records proof that a mandatory notice was delivered and the first day its effect is allowed. A policy does not lapse before the earliest effect of its latest lapse notice, so a corrected notice restarts the clock. A policy with no lapse notice on record lapses by the product's arrears rules alone.

## Modules beside the core

A module whose state spans partitions runs as its own Raft group on the ledger nodes (`ic_raft::module`), with the same durable log, snapshots, membership changes and failover as a partition. A module that reads the core's events is fed them by the leader of its group: every member is shown each event of each partition once, in sequence, and the checkpoint is part of the group's replicated state. Each partition keeps its events until they are on the stream and every registered reader has them. `docs/modules.md` is the contract.

## Privacy

Command payloads carry personal and health information. They never go to logs: a log line names a command, its sender and its key only. Ledger entries carry salted commitments to their payloads, so the chain can be verified without the payloads being readable from it.
