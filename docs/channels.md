# Portal and app services

`crates/channels` (`ic-channels`) is what people see: the customer app's API, the adviser portal, the employer portal, the staff console, the operations console and the public status page. It also holds the things those share: who a sign-in is, which devices a customer has enrolled, what each member of staff may do, what each person may see of a policy, and the held balance workbench. It is a deterministic state machine beside the core, built to the contract in `docs/modules.md`, plus a stateless HTTP service, `ic-portal`, in front of it.

## The shape of it

- **The channel module** (`state.rs`) runs as a Raft group named `channels` on the ledger nodes, registered in `crates/node`. It reads the core's events and is told the rest by the other modules. Nothing in it reads a clock: every command carries its time.
- **The portal service** (`http.rs`, `main.rs`, binary `ic-portal`) is a gateway with no state of its own. It checks the caller's token, asks the module a question or proposes a command, and answers. Any number of copies can run; losing one loses nothing.
- **The web pages** (`ui/`) are compiled into the service: `/adviser`, `/employer`, `/staff`, `/pricing`, `/complaints`, `/finance`, `/claims`, `/reinsurance`, `/ops`, `/status`. They load nothing from anywhere else.
- **The customer app** is not in this repository. Its whole contract is the `/app/v1` API below, including the offline sync protocol. The native app belongs with the bank's app.

## Who signs in, and how

Two tenants, never mixed:

- **Staff and other modules** sign in with the insurer's Microsoft Entra ID tenant. `/staff/v1`, `/ops/v1` and `/internal/v1` accept only its tokens.
- **Customers, advisers and employer administrators** sign in with a Microsoft Entra External ID tenant. `/app/v1`, `/adviser/v1` and `/employer/v1` accept only its tokens.

A token says who signed in, not what they may do. The module binds each sign-in subject to a party (`BindSubject`, sent by the identity service after verification), and every answer is worked out from that party's relationships: policyholder, life insured, adviser under an engagement, employer administrator, assignee. Roles on a customer token mean nothing here.

A session ends early when the module says so: staff offboarding, a revoked device and "sign out everywhere" all cut off tokens issued before that moment, without waiting for them to expire.

### Devices and signed approvals

A customer enrols a phone by its public key (ECDSA P-256, in the phone's secure hardware). The first device needs an identity verification reference; each later one is approved by a device already enrolled.

- **A lost phone.** A person who still has a device adds another from it. One who has none is verified again and the identity service enrols the new device as a recovery: every device they had is revoked and every session closed in the same step. A revoked device that calls again is answered `401 device_revoked`, which is the app's instruction to wipe what it holds.
- **Proof of device.** A request from the app carries `x-device-id` and `x-device-proof: <timestamp>.<signature>`, signed over the timestamp, method, path and the SHA-256 of the body. Offline data (`/app/v1/sync`) and challenges are given only to a device that proves itself.
- **Signing a change.** To approve something, the app asks for a challenge for the exact action (`POST /app/v1/challenges`), signs it on the device and sends the signature with the action. A challenge is for one action, one device, used once and short-lived. A signature for one change cannot approve another.
- **What needs it.** A request an adviser or a member of staff prepares for a customer waits in `awaiting_approval` until the customer signs it. Nobody approves for the customer.

### Writes are safe to repeat

Every write needs an `Idempotency-Key` header. The same key from the same person returns the first answer, marked as a repeat. Errors are RFC 9457 problem documents; `title` is a stable code.

## The customer app API

| Call | What it does |
| --- | --- |
| `GET /app/v1/me` | Who you are, your policies and your relationship to each, any notices. |
| `GET /app/v1/sync?have=<n>` | What the device needs since position `n`: changed rows, deleted rows, or `purge` when access was lost. Without `have`, a full snapshot. |
| `GET /app/v1/policies/{policy}` | The policy as you may see it. |
| `POST`, `DELETE /app/v1/policies/{policy}/advisers` | Appoint or remove an adviser. Removal takes effect at once. |
| `PUT /app/v1/policies/{policy}/delegates/{party}` | Let someone look (`viewer`, `financial` or `support`), change it, or end it with `null`. |
| `PUT /app/v1/policies/{policy}/consent` | An adult on a family policy lets the policyholder see their sensitive rows, or stops it. |
| `GET`, `POST /app/v1/requests`, `DELETE /app/v1/requests/{id}`, `POST /app/v1/requests/{id}/approve` | Ask for a change, withdraw it, approve one prepared for you. |
| `POST /app/v1/challenges` | A challenge to sign. |
| `GET`, `POST /app/v1/devices`, `DELETE /app/v1/devices/{device}` | Your devices. |

**Sync.** Each party has a feed of rows with a position that only goes up. A device keeps its position and asks for what is newer. A row that no longer exists comes as a delete; a party who lost access to a policy gets a purge for it, so nothing lingers on a phone. A device too far behind gets a snapshot instead. Rows carrying health or claim details about another adult are left out unless that adult consented.

**People the policyholder lets in.** A policyholder can let a relative look at a policy (`viewer`), let an accountant see its money side and documents (`financial`), or name a support person (`support`). Each has their own sign-in; nobody shares one. None of them ever sees a row that implies anything about health, and none can change anything. A support person can prepare a request, which then waits for the policyholder's own signature on their own device. The policyholder ends the access at any time and the other person's device is purged; it also ends if the policy changes hands.

**Children.** The customer master records each child insured under a policy with the date they reach the age the product sets for controlling their own health information (`RecordDependant`). Until that date the policyholder sees the child's claims and other health-implying rows as a parent does. From the first business date on or after it, those rows are purged from the policyholder's devices, a `control_passed` effect goes out so both are told, and the young person can enrol a device of their own and, if they wish, consent to the parent seeing them again.

**When the data may be stale.** The module knows when it last heard from the event stream. If that is more than two minutes ago, answers carry `fresh: false`, the audience policy is given that fact and withholds what it only allows on current relationship data, and the app shows "as at" rather than presenting old data as current.

**When an adviser is unavailable (addendum entry 4).** If the licence an adviser's engagement depends on is suspended or lost, the customer's policy shows the adviser with `state: "unavailable"`, the date, and the insurer as the contact, from the moment the status is recorded. The customer may appoint another adviser at any time.

## Adviser and employer portals

- `GET /adviser/v1/book`: the adviser's engagements, their standing, their book and what is held from them. When the licence they work under is suspended or lost, the book becomes **read-only**: they still see it, and nothing can be quoted, submitted or changed (addendum entry 4). An engagement that ended leaves nothing visible.
- `GET /adviser/v1/held-balance`: the payee's own held money and the reason for each amount, and nothing about anyone else's.
- `GET /employer/v1/schemes` and the two `PUT` calls under it: a scheme's administrators see its members and billing, name other administrators and choose which changes need a second administrator. A change of that kind waits for a different administrator to approve it.

## The staff console

Every staff action is put to four questions, answered by Cedar policy in `config/cedar/staff.cedar` over facts the module holds:

1. **What**: does a role this person holds include the action?
2. **Whose**: do they have a path to this record (a case, queue, team, scheme or book)?
3. **How much**: is the amount within their authority profile, capped by the role's ceiling?
4. **Why**: is there a purpose, such as the case they are working on?

A refusal says which question failed, so the person knows what to ask for. Roles are templates; authority profiles are versioned; grants are requested and approved by someone else, expire, and are recertified. Break-glass access is time-limited and reviewed afterwards. An external entitlement source can feed all of this; when it cannot be reached, what people already hold runs to its end and nothing new is granted from it.

**Data classes.** A view is shaped to the classes of data the person's roles include. Health and claim detail are withheld from someone without that class, and the person is told that something was withheld, never what.

**Personal records.** Nobody opens their own policy in the staff console, or the policy of someone they have declared a personal connection with, whatever their role and path. Staff declare connections themselves; only a staff administrator can remove one. The attempt is refused, logged and flagged.

**Flagged attempts.** An attempt on a record the person has no path to, or on a personal record, is marked in the access log and raised as an `access_flagged` event for the people who watch for misuse.

**The access log.** Every staff read of a policy or of the workbench is written to the access log before the data is returned, with the person, the record, the purpose and the decision. Refusals are logged too. If the log cannot be written the read fails. Staff see their own recent access at `GET /staff/v1/me`; nobody can write a line by hand.

| Call | What it does |
| --- | --- |
| `GET /staff/v1/me` | Your roles, reach, grants and recent access. |
| `GET /staff/v1/policies/{policy}?case=<ref>&scope=<keys>` | A policy as you may see it. |
| `POST /staff/v1/requests` | A change for a customer on the phone. It goes to the customer's app for their signed approval. |
| `POST /staff/v1/access`, `GET /staff/v1/admin` | Grants, approvals, recertification, administration. |
| `GET /staff/v1/workbench`, `/workbench/{party}`, `/workbench/{party}/proof-of-debt`, `POST /staff/v1/workbench/actions` | The held balance workbench. |

## The held balance workbench (addendum entry 4)

One place to see and resolve money held from an adviser, a provider or any other counterparty.

- **Positions come from the owning modules.** Commission, finance, reinsurance, group administration and claims each publish what they hold or are owed for a party. Compliance, the customer master and distribution record the party's status (active, suspended, licence lost, in liquidation, deceased). The workbench never invents a balance.
- **Actions**: release, offset against a receivable, hold pending a review date with a legal reference, write off, split, record a liquidator's dividend.
- **Maker and checker.** Whoever prepares an action cannot approve it. Above the two-approver threshold (NZ$50,000 by default) it needs two approvers and legal. Payment to a **new account** for a party in liquidation or deceased always needs legal, whatever the amount. So does a **release that would leave less held than another module is collecting** from the party (a claim overpayment that is `pursued` or `offset`), as the commission module also decides; a release prepared before such a debt appeared is refused there at its last signature (`owes_elsewhere`) and is prepared again. A write-off above NZ$1,000 needs a second approver. While a **conduct case** is open on the party, every action also needs the conduct officer (staff action `held_balance.approve_conduct`), a seat nobody else can fill. Thresholds are changed by a command of their own.
- **Daily control**: held money by age (30, 90, 180, 365 days and over) and reviews that have fallen due.
- **Proof of debt**: an extract of what the party owes, for a liquidator.
- **Carrying it out.** An approved action is handed to the module that owns the money, which reports back. The workbench records the outcome; it does not move money.

## The complaints and conduct workbench

`/complaints` is the staff screen for the complaints module (`docs/complaints.md`): recording a complaint wherever it is heard, the queue (vulnerable complainants first, then the nearest deadline), a complaint's file and running timers, every action on a complaint, referral to a forum, the register and the report for a period, and conduct cases with their population, sample and regulator requests.

That module takes each command from a named calling service. The workbench is those services for people, and a person's seats follow from their staff actions:

| Staff action | Seat (the caller that module sees) |
| --- | --- |
| `complaints.record` | `intake`: record a complaint, nothing else |
| `complaints.handle` | `complaints` and `intake`: everything on a complaint, and the register |
| `complaints.legal` | `legal`: forums and conduct cases |
| `complaints.conduct` | `conduct`: conduct cases |
| `complaints.vulnerability_trained` | no seat: marks the person as trained to work with vulnerable customers |

A person's **authority level** is the limit `complaints_authority` in their authority profile.

| Route | What it does |
| --- | --- |
| `GET /staff/v1/complaints/me` | Your seats, authority level and training. |
| `GET /staff/v1/complaints/{question}` | `queue`, `complaint?id=`, `running?id=`, `subject_record?id=`, `complaints_of?party=`, `register?from=&to=`, `report?from=&to=` for the complaints team, legal and conduct; `open_conduct_cases` and `conduct_case?id=` for conduct and legal only. |
| `POST /staff/v1/complaints/commands` | `{ "as": "<seat>", "command": { ... } }` in that module's own JSON, with an `Idempotency-Key`. |

What the service decides and the person cannot:

- **The seat.** A command goes as a seat the person holds or not at all. Commands only that module's own services send (`tick`, `mark_published`, `activate_timers`, the population commands) are refused whatever the seat.
- **Who is asking.** A complaint's file is asked for as the person signed in, and that module gives it only to someone the case has let in. Conduct cases are asked for in the role the person holds.
- **Authority and training.** `assign` carries the handler's authority level and training, and `record_remediation` the authoriser's level (the sender, unless the command names a colleague). Both are written in from the staff records, replacing anything sent. Someone who does not work complaints cannot be named.

That module's refusals come back as `409` with its own code; a file the case has not opened to the person is `403`; no leader is `503 complaints_unavailable`. The portal service holds no complaint data, and the two modules share no code: `tests/http.rs` runs the workbench against that module's own state machine.

## The unit pricing console

`/pricing` is the staff screen for the unit pricing module (`docs/unit-pricing.md`): the prices waiting for a person with their checks, valuation files that could not be read, prices overdue, the valuation received for a fund and date, prices in force, funds and their suspensions, and price errors with their corrections and remediation. From it a preparer prepares a price, an approver approves it (accepting each failed check with a reason when it is held) or rejects it, and a lead suspends and resumes a fund, raises and closes a price error, and offers a price again to partitions that refused it.

The portal service holds none of this. It passes the console's questions to the `unit-pricing` module on the nodes and sends each command there as the person who gave it, under the caller `unit-pricing-console`, with the roles their staff templates give them:

| Staff action | Role in the unit pricing module |
| --- | --- |
| `unit_pricing.view` | none: opens the console to read |
| `unit_pricing.prepare` | `unit_pricing_preparer` |
| `unit_pricing.approve` | `unit_pricing_approver` |
| `unit_pricing.lead` | `unit_pricing_lead` |

| Route | What it does |
| --- | --- |
| `GET /staff/v1/unit-pricing/me` | Whether you may open the console, and your roles. |
| `GET /staff/v1/unit-pricing/{question}` | One of `status`, `funds`, `work`, `errors`, `price?id=`, `error?id=`, `prices?fund=&from=&to=`, `valuations?fund=&pricing_date=`, `late?as_at=`. That module's other questions are not asked from here. |
| `POST /staff/v1/unit-pricing/commands` | One of `prepare_price`, `approve_price`, `reject_price`, `suspend_fund`, `resume_fund`, `raise_price_error`, `close_error`, `republish`, in that module's own JSON, with an `Idempotency-Key`. |

Someone without a unit pricing action is refused before anything reaches that module, and a command is refused here if the person lacks its role. That module checks the role again, enforces that an approver is never the preparer, and keeps who did what; its refusals come back as `409` with its own code (`403` for `unauthorised`). If it has no leader the console answers `503 unit_pricing_unavailable`. Valuation files arrive through the administrator's storage account, not the console. The two modules share no code; `tests/http.rs` runs the console against that module's own state machine.

## The claims console

`/claims` is the screen claims staff work from: the work queues across every partition, a claim as the claimant sees it (status, owner, decision due, evidence outstanding, admitted and paid), and every action a person takes on a claim, from a phone notification through assessment, decisions and their second signature, payees and payments, to overpayments and payment runs. It also shows the book of claims in payment, the management report and a run's remittance.

Claims live inside the core on the partition of each claim's policy (`docs/claims.md`), so the portal service talks to the core's partitions, not to a module of its own:

- **Who.** A member of staff whose templates hold `claims.work` gets in. Every command goes under the caller `claims`, and every `actor` in it is replaced with the person signed in and the purpose they typed, whatever the page sent. The purpose is required and recorded by the claims module. What a person may decide is the claims module's authority book, keyed by the same staff ID as the workforce directory; someone the book does not know is refused `403 unknown_staff`, and refusals for authority, ownership or access come back as `403` with that module's code, anything else as `409`.
- **Where.** A command naming a claim goes to the partition that holds it (`404 unknown_claim` when none does; a policy that is moving partition answers that briefly, so try again). A notification goes to the partition holding its policy. A payment run is on one partition, which the person names.
- **Not from people.** Configuration, acknowledgements (documents), bank outcomes and run confirmation (payments), deaths (customer master), reinsurance notices, non-disclosure outcomes (underwriting) and the controls are refused here whatever the person's roles.

| Route | What it does |
| --- | --- |
| `GET /staff/v1/claims/me` | Your staff ID and how many partitions the ledger has (`IC_PARTITIONS`). |
| `GET /staff/v1/claims/{question}` | `queue?queue=`, `claim?claim=` (the claimant's view), `work?claim=` (the staff view: tasks, evidence, the decision waiting for a signature, payee, payments, benefit, overpayments, timers), `book`, `report`, `remittance?run=&partition=`. |
| `POST /staff/v1/claims/commands` | `{ "purpose", "partition"?, "command" }`, the command in the claims module's own JSON, with an `Idempotency-Key`. |

`503 claims_unavailable` when the partitions have no leader. `tests/http.rs` runs the console against the claims module's own state machine on two partitions.

## The reinsurance exceptions console

`/reinsurance` shows the reinsurance module's open exceptions (`docs/reinsurance.md`) and what a person can do about each. That module is the source of truth: an exception closes itself once its cause is cured, so there is nothing to acknowledge. The portal reads the module itself, not the event hub the exceptions are also sent to.

| Exception | What a person does |
| --- | --- |
| `notice:<seq>` | Send the notice to claims again (`retry_notice`), or abandon it (`abandon_notice`, two people). |
| `delivery:<seq>` | Abandon the instruction (`abandon_instruction`, two people). The page warns that the core's ledger and the reinsurance ledger then stay apart by its amount. |
| `settlement_mismatch:<n>`, `settlement_returned:<n>` | Record the settlement with its payment reference and the statement's net (`record_settlement`). |
| `facultative_terms_altered:<policy>/<cover>/<layer>/<life>` | Record that the reinsurer was told, and where its answer is filed (`facultative_reinsurer_told`). |
| `claim_fact:...`, `possible_match:...` | Shown; they close when the facts arrive. |

Commands go under the caller `reinsurance-ops` on the latest business date any partition has reported. That module keeps no actor beyond an abandonment's `approved_by`, so the portal enforces who may act:

| Staff action | What it allows |
| --- | --- |
| `reinsurance.view` | Read the exceptions, staff notices, abandonments and the month-end check (`sweep_check`). |
| `reinsurance.operate` | Send what one person may (retry, settlement, reinsurer told) and prepare an abandonment. |
| `reinsurance.approve` | Sign an abandonment someone else prepared. |

**Two people.** An abandonment is prepared by one person and kept in the replicated channel state (`src/countersign.rs`) with its reason. A different person with `reinsurance.approve` signs it, and only then is it sent, with the signer as `approved_by`; the page cannot type an approver. The person who prepared it can take it back until it is signed. If the module cannot be reached when it is signed, it stays signed and the signer signs again to finish it; the module's key is the proposal's, so it applies once. A refusal from the module ends it, and it is prepared again if still needed.

| Route | What it does |
| --- | --- |
| `GET /staff/v1/reinsurance/me` | What you may do, and the abandonments waiting for a signature. |
| `GET /staff/v1/reinsurance/{question}` | `exceptions`, `notices`, `abandoned`, `abandoned_notices`, `sweep_check?period_end=`, `statement?number=`, `notice_retries?seq=`. |
| `POST /staff/v1/reinsurance/commands` | One of the commands above, in that module's own JSON, with an `Idempotency-Key`. An abandonment answers `{prepared}`. |
| `POST /staff/v1/reinsurance/sign/{id}`, `.../withdraw/{id}` | Sign and send, or take back, a prepared abandonment. |

## The finance console

`/finance` is the staff screen for the finance ledger (`docs/finance.md`): the month-end close dashboard for a period with its checks, waivers and sign-offs, work waiting, manual journals, the trial balance, movements, sources, backdated entries, conduct figures, returns and aged suspense. From it an operator proposes a journal, someone else approves or rejects it, the appointed actuary signs an actuarial journal and the period, the finance lead waives a check with its remediation owner and date, signs off and closes the period, approves a proposed configuration, and retries a stalled feed.

As with the other consoles, the portal service holds none of it. It passes the questions to the `finance` module on the nodes and sends each command as the person who gave it, under the caller `finance-console`:

| Staff action | Role in the finance ledger |
| --- | --- |
| `finance.view` | none: opens the console to read |
| `finance.operate` | `finance_operator` |
| `finance.approve` | `finance_approver` |
| `finance.lead` | `finance_lead` |
| `finance.actuary` | `appointed_actuary` |

| Route | What it does |
| --- | --- |
| `GET /staff/v1/finance/me` | Whether you may open the console, and your roles. |
| `GET /staff/v1/finance/{question}` | One of `status`, `work`, `dashboard`, `checks`, `trial_balance`, `movements`, `sources`, `backdated`, `journal`, `conduct`, `return`, `aged_suspense`, `extract`, with that question's fields as query parameters. |
| `POST /staff/v1/finance/commands` | One of that module's commands in its own JSON, with an `Idempotency-Key`. Commands only its own services send (the configuration, the core's event feed, Merkle roots, extracts, conduct facts, and the check and aged item reports other modules make) are refused. |

- **Amounts.** That module counts journal lines in millionths of a minor unit, more than a browser number holds exactly. A line may give `signed_minor` instead (debits positive, credits negative), which the service turns into `signed_fine` exactly, refusing a figure out of range.
- **Who may.** The ledger checks every role itself and keeps maker and checker apart whatever roles one person holds. A refusal for want of a role comes back as `403 authority`, any other as `409` with its code, and `503 finance_unavailable` when it has no leader.
- **Not on the page.** A chart of accounts, a close configuration and a return definition are proposed from their files through the command route; the page only approves them.

## Operations console and status page (addendum entry 6)

`IC_REGIONS` tells the service which ledger node is in which region and `IC_HOME_REGION` which region normally leads. From that and what the nodes report:

- `/ops` and `GET /ops/v1/regions` (role `operations`) show each region's reachable nodes and leaders, whether there is a majority, whether leaders are at home, and the channel module's own state.
- `/status` and `GET /status.json` need no sign-in and say it in plain words. **Operational**: everything works. **Degraded**: the service is running from the other region, or with a region out of reach; everything still works and may be slower. **Outage**: no majority; people may not be able to sign in, cover is not affected, and nothing already sent is lost.

The status page keeps answering when the ledger cannot be reached, because that is when people look at it.

## What other modules send and read

Modules call `/internal/v1/commands` with a token from the workforce tenant. The module's name goes in `x-module` and must be an app role on the token.

| Who | Sends |
| --- | --- |
| Identity service | `BindSubject`, `UnbindSubject`, device enrolment after verification. |
| Customer master, group administration, distribution | Ownership, assignments, children and their age of control, employer administrators, engagements and licence status. |
| Any module with rows to show | Rows for a policy, with a kind, a subject and whether they are sensitive. |
| Commission, finance, reinsurance, group administration, claims | Workbench positions, and the outcome of an approved action. |
| The module a request is for | `GET /internal/v1/pending` for requests accepted and waiting, then the outcome of each. |
| Entitlement source | Staff holdings, mappings and just-in-time elevation. |

The core's own events (issue, billing, lapse, cancellation, cover changes) arrive through the module host and need nobody to send them.

**The commission module is carried across by the portal service** (`forward.rs`). Every 15 seconds `ic-portal` asks the `distribution` module on the nodes which parties changed, reads each one's position and engagements, and records the engagements, licence history, workbench status, held lines, debts, documents and cases in the channels. It also carries the structure above the advice businesses (that module's `under` and `licensed_by` relations: all of them when it starts, then those touching a changed party), so a licence lost by a group reaches the advisers under its providers, and the open conduct cases, which bring the conduct officer into workbench approvals. The two modules share no code: the forwarder reads that module's answers as JSON, and `tests/forward.rs` checks the questions and answers against that module's own types. Everything it sends is safe to send twice, so a restart republishes once and two copies of the service do no harm.

**Approved actions on commission money go back the same way.** The commission module moves the money and checks authority itself, so an action approved on the workbench is replayed there as each person's own act: the maker's proposal, then each approver's signature, each sent as that person with the roles and limits they hold here. What that module makes of it (posted, refused, rejected, failed) is recorded as the action's outcome on the workbench.

## Running it

| Setting | Meaning |
| --- | --- |
| `IC_PORTAL_LISTEN` | Default `0.0.0.0:8090`. |
| `IC_NODES`, `IC_CLUSTER_TOKEN_FILE` | The ledger nodes and the token they require, as for `ic-api`. |
| `IC_ENTRA_TENANT_ID`, `IC_ENTRA_AUDIENCE`, `IC_ENTRA_CLIENT_ID` | The workforce tenant, the portal service's app registration, and the single-page application the staff and operations consoles sign in through. |
| `IC_CUSTOMER_AUTHORITY`, `IC_CUSTOMER_TENANT_ID`, `IC_CUSTOMER_AUDIENCE`, `IC_CUSTOMER_CLIENT_ID` | The same for the External ID tenant and the adviser and employer portals. |
| `IC_ENTRA_SCOPE`, `IC_CUSTOMER_SCOPE` | The scope the pages ask for. Default `<audience>/.default`. |
| `IC_REGIONS`, `IC_HOME_REGION` | Which node is in which region. Without them, one region. |
| `IC_LEDGER_PATIENCE_SECS`, `IC_PORTAL_HEARTBEAT_SECS`, `IC_PORTAL_FORWARD_SECS` | Defaults 10, 15 and 15. |
| `IC_PORTAL_CURRENCY` | The currency of the commission module's balances. Default `NZD`. |
| `IC_AUTH=dev` | Trust `x-dev-subject` and `x-dev-roles` headers. For a laptop. The service refuses to start with it in Kubernetes. |

**On AKS.** `deploy/k8s/portal.yaml` holds the service account, the settings, the deployment and its network policy; the identity is the `portalClientId` output of `infra/main.bicep`. In Entra, register:

1. In the workforce tenant: an app registration for the portal service, with the app roles `operations` and one per module that calls `/internal/v1`; and a single-page application for the consoles with redirect URIs `https://<host>/staff` and `https://<host>/ops`.
2. In the External ID tenant: an app registration for the customer-facing API, and a single-page application with redirect URIs `https://<host>/adviser` and `https://<host>/employer`. The mobile app has its own registration there.

Fill in the `REPLACE-` values in `portal.yaml`, apply, and route the public host to the `ic-portal` Service through the ingress controller. Put Azure Front Door with its web application firewall in front of the public host.

**On a laptop.** `docker compose -f deploy/compose/docker-compose.yml up --build -d` starts the service at `http://localhost:8090` with development sign-in.

**The pages in a browser.** The pages sign in with the authorization code flow and PKCE and keep the token for the tab only. The content security policy allows this service's own files, each page's inline script by its hash, and calls to this service and the sign-in authorities; nothing else runs and nothing can frame a page. To drive every page in a real browser against a seeded portal:

```sh
cargo test -p ic-channels --test http serve_for_a_browser -- --ignored &
node crates/channels/tests/browser/check.js
```

## Known gaps

- **No clinical view in the claims console.** Staff work from the claims module's staff view (`work`), which carries nothing clinical. The cause, reason codes and evidence documents stay in that module until it offers a question for them after `open_file`.
- **The native customer app is not here.** The API and sync protocol are complete and tested; the app itself belongs with the bank's app.
- **New business is not in the adviser portal.** Quotes and proposals are underwriting's API. The portal shows the book, standing, held money and servicing requests.
- **A scheme member sees their cover only once they are a known person.** A member who joins from a membership file without being named gets their feed when group administration or claims identifies them (`IdentifyMember`).
- **The browser check is not run by CI.** CI has no browser. Run it before changing anything under `ui/`.
- **Freshness depends on a heartbeat.** The gateway tells the module the stream is current every 15 seconds once it has confirmed the module is caught up. The heartbeat is a command, so it needs a majority; without one, answers go stale after two minutes and say so, which is the truth.
- **Claim overpayments are shown, not worked.** The commission module lists debts other modules keep (claim overpayments) and the forwarder shows them on the party's position as `memo/...` items, so the approver sees everything the party owes. They are recovered in the claims module; an offset against one here is refused by the commission module.
- **Only commission is carried across automatically.** Finance, reinsurance, group administration and claims must send their own workbench positions through `/internal/v1/commands`; until they do, the workbench shows none of their money.
- **Two sets of thresholds.** The workbench here and the commission module each decide how many signatures an action needs, from their own settings. They ship alike. If the commission module's are made stricter, an action approved here comes back as `needs_more_approval` and stays open there for the people it names.
- **The conduct officer here and there.** The workbench asks for the conduct officer whenever a conduct case is open on the party; the commission module asks by its own rules. The signature is replayed as `conduct_officer`, and where that module did not need it the action carries on with its other signatures.
- **Commission balances are taken to be in one currency** (`IC_PORTAL_CURRENCY`, default NZD), because that module's answers carry none.
