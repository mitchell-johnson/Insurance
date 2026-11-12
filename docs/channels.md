# Portal and app services

`crates/channels` (`ic-channels`) is what people see: the customer app's API, the adviser portal, the employer portal, the staff console, the operations console and the public status page. It also holds the things those share: who a sign-in is, which devices a customer has enrolled, what each member of staff may do, what each person may see of a policy, and the held balance workbench. It is a deterministic state machine beside the core, built to the contract in `docs/modules.md`, plus a stateless HTTP service, `ic-portal`, in front of it.

## The shape of it

- **The channel module** (`state.rs`) runs as a Raft group named `channels` on the ledger nodes, registered in `crates/node`. It reads the core's events and is told the rest by the other modules. Nothing in it reads a clock: every command carries its time.
- **The portal service** (`http.rs`, `main.rs`, binary `ic-portal`) is a gateway with no state of its own. It checks the caller's token, asks the module a question or proposes a command, and answers. Any number of copies can run; losing one loses nothing.
- **The web pages** (`ui/`) are compiled into the service: `/adviser`, `/employer`, `/staff`, `/ops`, `/status`. They load nothing from anywhere else.
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
| `PUT /app/v1/policies/{policy}/consent` | An adult on a family policy lets the policyholder see their sensitive rows, or stops it. |
| `GET`, `POST /app/v1/requests`, `DELETE /app/v1/requests/{id}`, `POST /app/v1/requests/{id}/approve` | Ask for a change, withdraw it, approve one prepared for you. |
| `POST /app/v1/challenges` | A challenge to sign. |
| `GET`, `POST /app/v1/devices`, `DELETE /app/v1/devices/{device}` | Your devices. |

**Sync.** Each party has a feed of rows with a position that only goes up. A device keeps its position and asks for what is newer. A row that no longer exists comes as a delete; a party who lost access to a policy gets a purge for it, so nothing lingers on a phone. A device too far behind gets a snapshot instead. Rows carrying health or claim details about another adult are left out unless that adult consented.

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
- **Maker and checker.** Whoever prepares an action cannot approve it. Above the two-approver threshold (NZ$50,000 by default) it needs two approvers and legal. Payment to a **new account** for a party in liquidation or deceased always needs legal, whatever the amount. A write-off above NZ$1,000 needs a second approver. Thresholds are changed by a command of their own.
- **Daily control**: held money by age (30, 90, 180, 365 days and over) and reviews that have fallen due.
- **Proof of debt**: an extract of what the party owes, for a liquidator.
- **Carrying it out.** An approved action is handed to the module that owns the money, which reports back. The workbench records the outcome; it does not move money.

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
| Customer master, group administration, distribution | Ownership, assignments, employer administrators, engagements and licence status. |
| Any module with rows to show | Rows for a policy, with a kind, a subject and whether they are sensitive. |
| Commission, finance, reinsurance, group administration, claims | Workbench positions, and the outcome of an approved action. |
| The module a request is for | `GET /internal/v1/pending` for requests accepted and waiting, then the outcome of each. |
| Entitlement source | Staff holdings, mappings and just-in-time elevation. |

The core's own events (issue, billing, lapse, cancellation, cover changes) arrive through the module host and need nobody to send them.

## Running it

| Setting | Meaning |
| --- | --- |
| `IC_PORTAL_LISTEN` | Default `0.0.0.0:8090`. |
| `IC_NODES`, `IC_CLUSTER_TOKEN_FILE` | The ledger nodes and the token they require, as for `ic-api`. |
| `IC_ENTRA_TENANT_ID`, `IC_ENTRA_AUDIENCE`, `IC_ENTRA_CLIENT_ID` | The workforce tenant, the portal service's app registration, and the single-page application the staff and operations consoles sign in through. |
| `IC_CUSTOMER_AUTHORITY`, `IC_CUSTOMER_TENANT_ID`, `IC_CUSTOMER_AUDIENCE`, `IC_CUSTOMER_CLIENT_ID` | The same for the External ID tenant and the adviser and employer portals. |
| `IC_ENTRA_SCOPE`, `IC_CUSTOMER_SCOPE` | The scope the pages ask for. Default `<audience>/.default`. |
| `IC_REGIONS`, `IC_HOME_REGION` | Which node is in which region. Without them, one region. |
| `IC_LEDGER_PATIENCE_SECS`, `IC_PORTAL_HEARTBEAT_SECS` | Defaults 10 and 15. |
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

- **The native customer app is not here.** The API and sync protocol are complete and tested; the app itself belongs with the bank's app.
- **New business is not in the adviser portal.** Quotes and proposals are underwriting's API. The portal shows the book, standing, held money and servicing requests.
- **A scheme member sees their cover only once they are a known person.** A member who joins from a membership file without being named gets their feed when group administration or claims identifies them (`IdentifyMember`).
- **The browser check is not run by CI.** CI has no browser. Run it before changing anything under `ui/`.
- **Freshness depends on a heartbeat.** The gateway tells the module the stream is current every 15 seconds once it has confirmed the module is caught up. The heartbeat is a command, so it needs a majority; without one, answers go stale after two minutes and say so, which is the truth.
- **Modules reach the workbench over HTTP today.** Each owning module must send its own positions; until it does, the workbench is empty for that module's money. Commission is the first publisher.
