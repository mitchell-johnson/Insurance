# Payments and bank channels

`crates/payments` (`ic-payments`) is money in and money out: direct debit authorities, payee accounts, payment and collection runs, refunds, bank statements and their reconciliation, and the evacuation drill. It is a deterministic state machine beside the core, built to the contract in `docs/modules.md` and hosted on the ledger nodes under the name `payments`. `crates/payments-service` (`ic-payments-service`) is everything around it that touches the outside world: the bank channels, the pump that carries submissions to the banks and their answers back, the evidence store, and a fake bank for tests.

No real bank is connected. The channels are built against the interfaces in the design notes and tested against the fake bank in `crates/payments-service/src/fake.rs`. The wire format is the module's own and is replaced per bank when a bank is chosen ("What is not built", below).

## What it does

- **Configuration through the log** (`config.rs`). Banks, channels (file or API, with a cutoff), bank accounts by purpose and statutory fund, routes (primary channel, secondary, urgent, approvals needed, item, run and day limits) and rules. Each channel lists an endpoint per region with its own credential name, signing key name, certificate expiry and egress address. A configuration that leaves any voter region without a working, whitelisted, unexpired endpoint on any channel is refused with `region_not_provisioned` (addendum entry 6).
- **Direct debit authorities** (`authority.rs`). Set up, amended, suspended and cancelled by a payee maintainer. Repeated dishonours suspend an authority. When the core records that a payer has died, the authority is cancelled at once and nothing more is collected under it.
- **Payee accounts** (`payee.rs`). A new or changed account is unverified until confirmed with the bank and then cools off for the configured hours before it can be paid. Estate and liquidator payees also need legal sign-off. A blocked account cannot be added under another name.
- **Runs** (`run.rs`). A maker prepares a run of one of twelve types, adds items, and checks it. Checks cover payee and authority status, duplicates within the window, limits and available funds. An approver who is not the maker, and who holds no payee or connectivity role, approves it; large runs need two. Approval queues one submission per channel. Items over the pre-advice threshold notify treasury.
- **Submission, exactly once.** A submission is claimed in the module before anything is sent, signed, kept as evidence, and then sent under its own ID. The bank takes an ID once. A send whose outcome is unknown is never repeated: after a grace period the bank is asked whether it holds the batch. A file the bank did not receive is replaced by a new submission with a new ID.
- **Fallback and redirection.** A channel that refuses falls back to the route's secondary channel. A connectivity administrator can redirect a route to the second bank while the first is out.
- **Acknowledgement, settlement and returns.** Each is recorded against its item. Settlement tells the core to settle the payable or receipt the collection; a return reverses it, counts a dishonour, and where the rules allow queues one re-presentment.
- **Refunds** (`refund.rs`). A refund goes back to the account the money came from unless a maker gives a reason and an approver agrees. After the payer's death the source is no longer used: the refund waits for the claim and is paid with it, to whoever the claim pays (addendum entry 1).
- **Reconciliation** (`recon.rs`). Statement lines are matched to what was expected. Anything else becomes an exception that a reconciler takes and resolves with a reason.
- **The evacuation drill** (`drill.rs`). From each region, a token payment through every channel, signed and sent exactly as a real one is. The `readiness` question says, per region, whether a drill passed within the configured age.
- **Fail-back waits for submissions.** The module answers `{"failback_quiet": true}` with `false` while any submission is being sent, so leadership is not moved under a file in flight.
- **Refusals leave no trace.** A refused command changes nothing; a repeated key gets its first result.

## How it talks to the rest

In: an `Envelope` with `key` (the idempotency key), `actor` (`id` and `roles`), `at` (New Zealand wall-clock time) and `command`. Roles are `maker`, `approver`, `payee_maintainer`, `connectivity_admin`, `legal`, `reconciler`, `treasury` and `service`. The caller authenticates the person and states the roles; the module enforces who may do what, including that an approver holds no payee or connectivity role.

Other modules ask for money to move with two commands, both as a `maker`:

- `add_run_items` on a run they prepared with `prepare_run`: each item names its counterparty (a payee or an authority), an amount, and a `source` of the module's choosing that comes back on every event about the item.
- `request_refund` for a premium refund on a policy.

Out, on each applied command: events with a hash-chained sequence (question `trail`), and an outbox of instructions the pump works through in order. `item_settled` and `item_returned` carry the item's `source`; that is how claims, commission and reinsurance learn what happened to a payment they asked for. Instructions to the core settle or return a policy payable, raise or release a refund payable, or ask for a receipt to be matched.

Questions (`{"q": "..."}`): `config`, `run`, `submission`, `batch`, `queued_submissions`, `unconfirmed_submissions`, `submission_in_progress`, `control_totals`, `authority`, `payee`, `refund`, `represent`, `reconciliation`, `readiness`, `outbox` and `trail`.

## Running it

The module needs nothing: it is in the node binary. The pump is `ic-payments-pump`, deployed by `deploy/k8s/payments.yaml`, two replicas in each voter region.

1. **Platform.** `infra/main.bicep` creates the pump's identity, a payments vault that only that identity can use, and the immutable `payments-evidence` container. Fill the `REPLACE-` values in `deploy/k8s/payments.yaml` from the outputs `paymentsClientId`, `paymentsKeyVaultName`, `paymentsKeyVaultUrl`, `keyVaultName`, `blobEndpoint` and `tenantId`, and set `IC_REGION` to the region's name as the payments configuration will name it.
2. **Per bank channel and region**, as the connectivity administrator: create the signing key in the payments vault (`az keyvault key create --vault-name <paymentsKeyVaultName> --name <channel>-<region>-signing --kty EC-HSM --curve P-256 --ops sign verify`), store the credential the bank issued as a secret, give the bank the public key and the region's egress address to whitelist, and list the secret in the `ic-payments-bank-credentials` class.
3. **Activate the configuration** with `activate_config`. It is refused until every voter region is provisioned on every channel.
4. **Run a drill from each region** (`kubectl create job --from=cronjob/ic-payments-drill first-drill`) before the first real run.

Settings: `IC_REGION`, `IC_BANK_SECRETS_DIR`, `IC_KEY_VAULT_URL`, `IC_BLOB_ENDPOINT`, `IC_PAYMENTS_CONTAINER` (default `payments-evidence`), `IC_PUMP_INTERVAL_SECS` (15), `IC_PUMP_GRACE_MINUTES` (10, at least 2), and the cluster settings every client takes. The pump does not start without a vault, an evidence store and its credentials directory: there is no unsigned or unrecorded mode.

## Runbook

- **A submission shows `failed`.** The bank refused it or did not receive it. Read the reason on the submission. Never send the same file again by hand. A maker issues `resubmit`, which makes a new submission with a new ID; the old one stays on record as failed.
- **A submission stays `sending`.** The pump lost the connection mid-send. After the grace period it asks the bank whether it holds the batch and records the answer. If the bank cannot be asked, it stays `sending` and blocks fail-back; when the bank confirms by another route, the service role records `record_submitted` or `record_not_received`.
- **A bank is out.** The pump logs `not done this pass` with the channel. Where the route has a secondary channel it is used without anyone acting. To move a route to the second bank for the duration, a connectivity administrator issues `redirect_route` for the run type with `secondary: true`, and again with `secondary: false` to end it.
- **The drill job failed.** The job's log names the channel. Check, for that region: the certificate expiry, that the bank still whitelists the egress address, and that the credential secret is current. Readiness for the region stays stale until a drill passes.
- **A region is lost.** Pumps in the surviving regions carry on with their own endpoints, keys and credentials. Nothing is reconfigured. Submissions the lost region had claimed and not confirmed are asked about after the grace period by whichever pump is left.
- **A problem instruction.** If the core refuses an instruction, the action is marked failed and a reconciliation exception is raised for a person. If the core cannot be reached, the outbox waits in order.
- **Proving what was sent.** `payments-evidence/payments/<submission>/file.txt` and `manifest.json` hold the exact bytes and the signed manifest. The submission in the module holds the same content hash.

## What is not built

- **A real bank's formats and transport.** Channels are HTTPS with a bearer credential and a signed manifest, in the module's own batch format. A real bank brings its own file layout (for example ISO 20022 or a domestic direct credit format), SFTP or a host-to-host API, and mutual TLS. `Bank` in `bank.rs` is the one trait to implement per bank; the certificate expiry in the configuration is checked but no client certificate is presented.
- **Durable message cursors.** The pump remembers in memory how far it has read each bank's messages. After a restart it reads from the start; every message is recorded under an idempotent key, so replays change nothing, but a bank with a long history costs a slow first pass.
- **Drill payments on the statement.** A drill's token payments arrive on the bank statement as lines the module does not expect, and are resolved as exceptions by a reconciler. They are not posted to the ledger.
- **Postings for non-policy payments.** Policy payables and collections are posted in the core. Commission, reinsurer and supplier payments are signalled by `item_settled` and `item_returned` to the module that asked; the ledger entries for them belong to those modules and finance.
- **Account details are held in the module's state** in clear, as the bank needs them. They are not sealed per person, so they cannot be destroyed separately from the log.
- **Notices go to the structured log** (`payments.notice`) with their role. Delivery to a work queue or to people is the channels and documents modules' part and is not wired.
- **Receipt matching.** A `match` instruction hands an inbound credit to the core's cash matching with the bank reference; resolving the payer from an authority before matching is not done.
- **Egress addresses are recorded, not enforced.** The configuration records the address each region presents to the bank. Giving each region's pump a fixed egress address is the network's job when the second region is provisioned.
