# Portal and app services

`crates/channels` (`ic-channels`) is what people see: the customer app's API, the adviser portal, the employer portal, the staff console, the operations console and the public status page. It also holds the things those share: who a sign-in is, which devices a customer has enrolled, what each member of staff may do, what each person may see of a policy, and the held balance workbench. It is a deterministic state machine beside the core, built to the contract in `docs/modules.md`, plus a stateless HTTP service, `ic-portal`, in front of it.

## The shape of it

- **The channel module** (`state.rs`) runs as a Raft group named `channels` on the ledger nodes, registered in `crates/node`. It reads the core's events and is told the rest by the other modules. Nothing in it reads a clock: every command carries its time.
- **The portal service** (`http.rs`, `main.rs`, binary `ic-portal`) is a gateway with no state of its own. It checks the caller's token, asks the module a question or proposes a command, and answers. Any number of copies can run; losing one loses nothing.
- **The web pages** (`ui/`) are compiled into the service: `/adviser`, `/employer`, `/staff`, `/pricing`, `/complaints`, `/finance`, `/claims`, `/reinsurance`, `/underwriting`, `/payments`, `/ops`, `/status`. They load nothing from anywhere else.
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

A policy carries its core status and `ended`, true for anything that is no longer live: lapsed, surrendered, matured, expired, cancelled, ended by a claim, transferred away or avoided for non-disclosure. An avoided policy is treated as never in force, so its covers show as not in force too.

| Call | What it does |
| --- | --- |
| `GET /app/v1/me` | Who you are, your policies and your relationship to each, any notices. |
| `GET /app/v1/sync?have=<n>` | What the device needs since position `n`: changed rows, deleted rows, or `purge` when access was lost. Without `have`, a full snapshot. |
| `GET /app/v1/policies/{policy}` | The policy as you may see it. |
| `POST`, `DELETE /app/v1/policies/{policy}/advisers` | Appoint or remove an adviser. Removal takes effect at once. |
| `PUT /app/v1/policies/{policy}/delegates/{party}` | Let someone look (`viewer`, `financial` or `support`), change it, or end it with `null`. |
| `PUT /app/v1/policies/{policy}/consent` | An adult on a family policy lets the policyholder see their sensitive rows, or stops it. |
| `GET`, `POST /app/v1/requests`, `DELETE /app/v1/requests/{id}`, `POST /app/v1/requests/{id}/approve` | Ask for a change, withdraw it, approve one prepared for you. Every write answers `202` with `version`, your own feed's position once it is recorded: sync until you hold at least that and you read your own write. The answer never names anyone else whose view moved with it. |
| `POST /app/v1/challenges` | A challenge to sign. |
| `GET`, `POST /app/v1/devices`, `DELETE /app/v1/devices/{device}` | Your devices. |
| `GET /app/v1/profile` | Your own record from the customer master: identity, contact, tax and organisation fields, with its version. |
| `POST /app/v1/profile/commands` | From an enrolled device: change your contact details (`{"command":"record","fields":{...}}`) or a consent (`set_consent`); put a hold on your own money going out (`place_spending_hold`), ask for it to end (`request_hold_removal {verification}`, naming a passed identity check made since the hold and in the last 24 hours, else 422 `verification_needed`) or take that request back (`cancel_hold_removal`). Send `If-Match` with the version you saw. Only legal lifts a hold at once; that command is refused here. |
| `GET /app/v1/profile/spending-hold` | Where your own hold stands: `{held, placed_at, removal: {requested_at, ends_at} or null}`. |
| `GET /app/v1/profile/export` | From an enrolled device: a copy of everything held about you (a Privacy Act access request). |
| `GET /app/v1/documents/{id}/pdf` | One of your own documents (one you own or were sent a copy of), as a PDF. Anyone else's is 404, the same as one that does not exist. |
| `GET /app/v1/proposals/{proposal}/lives/{life}/consents/{kind}/to-sign` | For the life on that proposal: the exact bytes to sign to give a consent (`ic.underwriting.consent.v1`, proposal, life and kind joined by zero bytes, base64), and whether the kind needs signing. Kinds: `adviser_access`, `existing_cover_prefill`, `obtain_records`, `employer_sees_loading`, `replacement_disclosure`. |
| `POST /app/v1/proposals/{proposal}/lives/{life}/consents/{kind}` | From an enrolled device, `{granted, signature}`: give or withdraw that consent. `adviser_access`, `employer_sees_loading` and `replacement_disclosure` are given only with the device's signature over that message, checked here (400 `bad_consent`) and again by the underwriting module, which takes it from the caller `customer-app` and refuses it with its own code otherwise (409, such as `replacement` when the proposal is not one). The replacement disclosure's figures come from underwriting's `proposal` and the core's view of the policy in force; its wording is placeholder text for legal review (documents' template `uw-replacement-disclosure`), not yet approved for issue. |
| `GET /app/v1/proposals/{proposal}/lives/{life}/consents/replacement_disclosure/statement` | For the life, before they sign the replacement disclosure: `replaced_policy`, `kept` (their cover in force on it, which stays), `gained` (the cover and sum insured the proposal adds), `lost` (empty: nothing held is ended), `old_premium` (the latest premium notice) and `new_premium_minor` (the quote). The added cover and quote come from a `replacement` object `{cover, sum_insured_minor, premium_minor}` on the `proposal_life` row underwriting publishes; the rest from the core. 409 `replacement` if the proposal replaces nothing; 404 for anyone but the life. These are the detail fields of the documents template `uw-replacement-disclosure`. Where the row's `replacement.replaced_policy` names another policy of ours, that policy's cover on the life is `lost` and nothing is `kept`. |
| `POST /app/v1/proposals/{proposal}/lives/{life}/disclosures/to-sign` | For the person insured on that proposal: what to sign for their answers (`{step, answers, adviser_entered}`, `step` being `disclosure` or `interview`). The underwriting intake checks the answers and keeps nothing. |
| `POST /app/v1/proposals/{proposal}/lives/{life}/disclosures` | From an enrolled device: the same answers with `message` (as `to-sign` gave it) and `signature` (the device key's, over those bytes). |

**Your record.** The profile routes call the customer master (`docs/customers.md`) with the portal's own Entra token (`IC_CUSTOMERS_URL`, `IC_CUSTOMERS_SCOPE`; both or neither, https unless on localhost). The portal sends only the signed-in party, as both party and actor, with purpose `self_service` or `subject_access`; the master's Cedar policy decides what that shows and allows, and its answer, including a 412 for a stale version, comes back as it is. The portal adds two checks of its own: a change or an export needs a device that proved its key, and only `record` and `set_consent` are sent. The idempotency key is scoped to the person. A portal deployed without the master, or a master that is down or answers 503, gives 503 `customer_master_unavailable`.

**Signing a disclosure.** The portal relays to the underwriting intake (`docs/underwriting.md`, "The disclosure intake") with its own Entra token (`IC_UNDERWRITING_INTAKE_URL`, `IC_UNDERWRITING_INTAKE_SCOPE`; the portal's identity holds the intake's app role `new-business`). It checks two things the intake cannot: that the person signed in is the life on the proposal, from the `proposal_life` row underwriting publishes with that person as its subject (anyone else gets 404); and that the signature is the proven device's own over the message, which it checks before anything is sent (400 `bad_signature`). It passes the device's enrolled P-256 key as `{key_id, public_key, signature, alg: "ecdsa-p256"}`, and the intake and the module check it again. The intake's answers (422 with the problems, 404, 409 with the module's code) come back as they are; an intake not configured or unreachable is 503 `underwriting_intake_unavailable`. Answers pass through and are never kept or logged here.

**Requests that go to a person first.** When the customer master is configured, each request a person submits asks the master's routing (`GET /v1/parties/{id}/routing`). Where it says `route_to_person`, the request carries `route_to_person: true`, so the module that owns it sends it to a trained person before anything automated happens. Nothing about why is carried. If the master cannot answer, the request is marked too. A change an adviser or support person prepares is asked for again when the customer approves it, with the customer's answer: the request goes to a person if either says so. A second administrator's approval of a scheme's change does not mark it.

**Data classes on the staff policy view.** The view is built with `classes::Shaper`, whose only way to add a field takes the field's data class; a field the member of staff does not hold the class of is left out and named under `masked`. A new field with no class does not compile (a `compile_fail` doctest on `Shaper::field`, run by `cargo test -p ic-channels --doc`).

**How to serve a customer, on the staff policy view.** When staff open a policy (`GET /staff/v1/policies/{policy}`) and the master's routing says the policyholder is served with care, the view carries `approach: {adjustments, review_on}`. The screen shows it as "Serve this customer with: ..." and never as a label on a list or header. Why (the marker's kind) is added only for staff holding `service.vulnerability_kind`, a specialist's action, and only when the master gives it to the portal. Nothing is added for anyone else. If the master cannot answer, `approach` is `{unavailable: true}` and the screen says it could not be checked.

**Sync.** Each party has a feed of rows with a position that only goes up. A device keeps its position and asks for what is newer. A row that no longer exists comes as a delete; a party who lost access to a policy gets a purge for it, so nothing lingers on a phone. A device too far behind gets a snapshot instead. Rows carrying health or claim details about another adult are left out unless that adult consented. A scheme's administrator sees the scheme in the employer portal only: what they see as administrator never goes to a phone, so a director who is also a member, or holds their own policies, carries only their own cover from the scheme.

**People the policyholder lets in.** A policyholder can let a relative look at a policy (`viewer`), let an accountant see its money side and documents (`financial`), or name a support person (`support`). Each has their own sign-in; nobody shares one. None of them ever sees a row that implies anything about health, and none can change anything. A support person can prepare a request, which then waits for the policyholder's own signature on their own device. The policyholder ends the access at any time and the other person's device is purged; it also ends if the policy changes hands.

**Children.** The customer master records each child insured under a policy with the date they reach the age the product sets for controlling their own health information (`RecordDependant`). Until that date the policyholder sees the child's claims and other health-implying rows as a parent does. From the first business date on or after it, those rows are purged from the policyholder's devices, a `control_passed` effect goes out so both are told, and the young person can enrol a device of their own and, if they wish, consent to the parent seeing them again. When the master ends a child's record (`EndDependant`), the policyholder stops seeing those rows at once.

**Units.** On a unit-linked policy the channel module follows the investment ledger's order events: a placed order is a `unit_order` row (`{policy}/unit_order/{id}`: fund, buy or sell, amount, kind, pricing date) until it executes or is cancelled, and each fund the policy has held is a `holding` row (`{policy}/holding/{fund}`: `units_micro`, `as_at`, the pricing date of the last order that moved it), adjusted by replays onto a corrected price. Once the core has activated a price for the fund, a holding row also carries `price`, `price_date` and `value_minor`: the units at the fund's latest sell price (what a withdrawal or surrender pays), from the core's `unit_price_activated` event, keeping the highest version of the latest pricing date. A value never appears without its price date, and a correction to an earlier date's price does not replace a later date's. A new price moves the feed of every policy holding the fund. Events from before prices were carried leave the holding as units only. They go to whoever sees the policy, so the policyholder's phone and the adviser page (section "Investments") show them.

**Owners, one or several.** A policy has a set of owners, each a policyholder in the app. The core's issue sets the first; `RecordOwnership` from the customer master adds an owner (a joint owner, or a buyer), and `EndOwnership {policy, party}` removes one. A transfer is the master recording the new owner and ending the old, in either order. When an owner leaves, they lose everything but their own cover, every adviser they authorised loses access, and anyone let look at the policy loses access (who let them in is not kept, so a remaining owner lets them in again). With no owner left the policy has none in the app, as when the only owner has died. An ending for someone who is not an owner does nothing.

**Money needs a signature.** A person's own request of a kind in `MONEY_KINDS` (`make_payment`, `change_payment_method`, `change_bank_account`, `withdrawal`, `surrender`) is refused `step_up_required` (401) unless their enrolled device signed exactly that request over a fresh challenge. A challenge needs a connection, so a payment keyed in offline cannot be queued and sent later unsigned. Reading offline from the device's own store is the native app's part.

**Every joint owner signs.** On a policy with more than one owner, a request an owner makes waits (`awaiting_approval` from the customer) until every owner has signed it on their own device; `signed_by` lists those who have. An owner who signs twice is told `already_signed`. A request prepared by an adviser or support person needs every owner too. Either owner may alone make the requests in `EITHER_OWNER` (`notify_claim`, `update_contact_details`).

**When the data may be stale.** The module knows when it last heard from the event stream. If that is more than two minutes ago, answers carry `fresh: false`, the audience policy is given that fact and withholds what it only allows on current relationship data, and the app shows "as at" rather than presenting old data as current.

**When an adviser is unavailable (addendum entry 4).** If the licence an adviser's engagement depends on is suspended or lost, the customer's policy shows the adviser with `state: "unavailable"`, the date, and the insurer as the contact, from the moment the status is recorded. The customer may appoint another adviser at any time.

## Adviser and employer portals

- `GET /adviser/v1/book`: the adviser's engagements, their standing, their book and what is held from them. When the licence they work under is suspended or lost, the book becomes **read-only**: they still see it, and nothing can be quoted, submitted or changed (addendum entry 4). An engagement that ended leaves nothing visible.
- `GET /adviser/v1/practice`: for an advice provider, each adviser engaged with it today and the policies they service; for a group, the same through every provider linked under it on the date (`Directory::practice`). Each policy is a summary as its adviser sees it, never rows, health facts or claims. Empty for anyone else.
- `GET /adviser/v1/statements/{number}`: the signed-in party's own commission statement from the commission module (the payee is always the person signed in), with its lines and balances and `totals` by what earned each line (`base`, `override:{level}`, `fee:{name}`), so a group sees its overrides and totals. Lines carry policies and amounts, never a claim or its cause. 404 until that statement is cut.
- `GET /adviser/v1/held-balance`: the payee's own held money and the reason for each amount, and nothing about anyone else's.
- `GET /adviser/v1/replacements`: the layers this adviser wrote, under any engagement, that the commission module's replacement rule matched to cover given up on the same life, with the matched amount, the reason and where each stands. The cover it replaced is left out, since it may be another adviser's business.
- `POST /adviser/v1/replacements/dispute {layer, reason}`: dispute one of those matches. The portal checks the layer is the adviser's own and not already disputed, records the dispute and its reason as the adviser's request (`dispute_replacement`, ID `dispute:<layer>`, owned by `distribution`, no customer's approval), sends the commission module `DisputeReplacement {policy, cover, layer, reference: the request ID, adviser}` under its own caller `adviser-portal` (that module checks again that the adviser wrote the layer), and resolves the request with the answer. A refusal comes back as 409 with the module's code. The general request route refuses that kind, so the check cannot be bypassed.
- `GET /app/v1/policies/{policy}/claims`: for the policy's adviser or a scheme's administrator, each claim on it with its status and the dates it was notified and closed, never its cause (Mitchell, 13 November 2026). The portal checks the relationship first; anyone else gets 404. It asks every partition the claims module's `PolicyStatus`.
- `GET /employer/v1/schemes` and the two `PUT` calls under it: a scheme's administrators see its members and billing, name other administrators and choose which changes need a second administrator. A change of that kind waits for a different administrator to approve it.
- **The scheme on the group service** (`src/group.rs`, `docs/group.md`). Under `/employer/v1/schemes/{policy}/group/`: `POST files/{file}` uploads a membership file (the body as it is, at most 32 MB) to quarantine; `GET file?file=` reads its preview; `POST commands` sends `approve_file`, `withdraw_file` or `declare_pool`; `GET scheme` and `GET invoices` read the scheme's standing and invoices; `GET member?employee=` reads one member as the employer sees them: category, salary, hours, billing group, dates, cover by formula and automatic amount, and whether cover above the automatic amount is in place. The portal first checks the person administers the policy today, then asks the group service `scheme_of`; anyone else, and a policy with no scheme, gets 404. The scheme on every call is the one found, never the page's; the actor is the signed-in administrator; the caller is `employer-portal`, the app role the portal's token carries. A member's record keeps to a fixed list of fields (`group::MEMBER_FIELDS`), so a loading, an exclusion, a reason or who the person is never reaches an employer even if the service sent one. Group keeps no premium per member, so none is shown. Staff's commands and questions (open exceptions, the mapping) are refused at the portal. Settings `IC_GROUP_URL` and `IC_GROUP_SCOPE`; without them these routes answer 503.

- **Documents on the pages.** The adviser page lists the policy's `document` rows (published by the documents module) with a button that downloads the PDF through `/app/v1/documents/{id}/pdf` as the signed-in person. Whether they get it is the documents module's rule.

## The staff console

Every staff action is put to four questions, answered by Cedar policy in `config/cedar/staff.cedar` over facts the module holds:

1. **What**: does a role this person holds include the action?
2. **Whose**: do they have a path to this record (a case, queue, team, scheme or book)?
3. **How much**: is the amount within their authority profile, capped by the role's ceiling?
4. **Why**: is there a purpose, such as the case they are working on?

A change to `config/cedar/staff.cedar` needs two people. `config/cedar/staff.approval.json` lists every approved version by hash, with who proposed it, the security approver (someone else) and how many sets of facts the version opens that the one before refused; `staff.approved.cedar` holds the version before the last. `tests/staff.rs::a_policy_change_needs_a_security_approver` fails the build while the file's hash is not the last entry or the stated count is wrong, and prints the cases opened (`StaffPolicy::widened_from` tries every context: twelve facts, five classes, four purposes). To change the policy: copy the current file over `staff.approved.cedar`, edit `staff.cedar`, run the test to learn the count, and append the entry with the approver's name. The first entry's approver is a placeholder until a security approver is named.

A refusal says which question failed, so the person knows what to ask for. Roles are templates; authority profiles are versioned; grants are requested and approved by someone else, expire, and are recertified. Break-glass access is time-limited and reviewed afterwards. An external entitlement source can feed all of this; when it cannot be reached, what people already hold runs to its end and nothing new is granted from it. An entitlement the mapping does not name confers nothing and is listed in the alignment report (on `GET /staff/v1/admin`), with the member of staff every mapping must name to work its exceptions (`alignment_owner`).

**Data classes.** A view is shaped to the classes of data the person's roles include. Health and claim detail are withheld from someone without that class, and the person is told that something was withheld, never what.

**Personal records.** Nobody opens their own policy in the staff console, or the policy of someone they have declared a personal connection with, whatever their role and path. Staff declare connections themselves; only a staff administrator can remove one. The attempt is refused, logged and flagged.

**Flagged attempts.** An attempt on a record the person has no path to, or on a personal record, is marked in the access log and raised as an `access_flagged` event for the people who watch for misuse.

**The access log.** Every staff read of a policy or of the workbench is written to the access log before the data is returned, with the person, the record, the purpose and the decision. Refusals are logged too. If the log cannot be written the read fails. Staff see their own recent access at `GET /staff/v1/me`; nobody can write a line by hand.

| Call | What it does |
| --- | --- |
| `GET /staff/v1/me` | Your roles, reach, grants and recent access. |
| `GET /staff/v1/policies/{policy}?case=<ref>&scope=<keys>` | A policy as you may see it. |
| `POST /staff/v1/requests` | A change for a customer on the phone, with `reason` and `case`; the request keeps who entered it with them. The enterer needs `service.enter_change`. One whose payload moves more than their `assisted_change` limit (`amount_minor`) or takes effect before today (`effective`) waits for a second member of staff first; then it goes to the customer's app for their signed approval. |
| `POST /staff/v1/requests/{id}/second-approval` | The second member of staff agrees it: someone else holding `service.approve_change`. |
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

### Replacement matches

`/replacements` lists every layer the commission module's replacement rule matched to cover given up on the same life (`docs/distribution.md`, "Replacements nobody declared"): who wrote it, the matched amount, the reason, the cover it replaced, and whether it stands, is disputed or was overturned. Anyone with `held_balance.view` reads it. A disputed match is decided by the conduct officer (`held_balance.approve_conduct`), with a purpose, through `POST /staff/v1/replacements/decide {policy, cover, layer, replacement, purpose}`. The portal sends `decide_replacement` to the commission module as that person, described as the workbench describes them (the conduct officer's template carries the role `conduct_officer`), and that module checks `case.manage` again by its own policy. Its refusals come back with their codes: 403 for `not_authorised`, 409 for anything else (`unknown_layer`, `not_matched`, `not_disputed`).

| Call | What it does |
| --- | --- |
| `GET /staff/v1/replacements` | Every match, and whether you may decide. |
| `POST /staff/v1/replacements/decide` | Decide a disputed match: `replacement: true` takes the held commission back; `false` makes the layer new business from now. |

### Documents

`GET /staff/v1/documents/{id}/pdf?purpose=...` downloads a document for staff holding `documents.download`, with the purpose logged by the documents module. Clinical documents are refused (403) and stay with the claims team's tools. Customers use `GET /app/v1/documents/{id}/pdf`. Both call the documents service (`docs/documents.md`, "Downloading") with the portal's Entra token (`IC_DOCUMENTS_URL`, `IC_DOCUMENTS_SCOPE`; app roles `portal` and `staff`), naming the party, or the staff member and purpose. The PDF is passed back with `Cache-Control: no-store` and never kept. A destroyed document is 410; the service not configured or unreachable is 503.

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

## The commission console

`/commission` is the staff screen for the commission module (`distribution`, `docs/distribution.md`), in `src/commission.rs`. Staff actions here give that module's roles: `commission.structure` gives `distribution_admin`, `commission.ops` gives `commission_ops`, `commission.approve` gives `commission_approver`, `commission.compliance` gives `compliance`; `commission.view` opens the console with no role. The workbench actions give the counterparty, legal and conduct roles as before. The person goes to that module as its own description of a member of staff (roles, profile limits, purpose), built here, and that module decides each command with `config/cedar/distribution.cedar` and records the decision.

| Route | What it does |
| --- | --- |
| `GET /staff/v1/commission/me` | Who you are and the roles you hold there. |
| `GET /staff/v1/commission/{question}` | `status`, `open_tasks`, `history`, `conduct_cases`, `relations`, `sweep_check`, `position` and `proof_of_debt` (`party`, `as_at`), `statement` (`payee`, `number`), `held_control`, `month_end_blocks` and `clawback_exposure` (`date`), `aged_held` (`as_at`), `action` and `case` (`id`). Anything not found is 404. |
| `POST /staff/v1/commission/commands` | `{purpose, command}`: one of `add_party`, `relate`, `end_relation`, `start_engagement`, `end_engagement`, `record_licence`, `mark_ceased`, `register_payee`, `add_payee_account`, `set_payment_hold`, `set_accrual_hold`, `add_payment_instruction`, `remove_payment_instruction`, `propose_change`, `approve_change`, `reject_change`, `cut_statement`, `attach_document`, `complete_task`, `close_case`, with an `Idempotency-Key`. That module's refusal comes back with its code (`not_authorised` is 403). Workbench actions, replacement decisions and policy assignment are refused here: they have their own pages and paths. |

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
| `GET /staff/v1/claims/definitions` | Every definition and version a cover can be bound to: ID, version, title, in force from and the hash of its text. |
| `GET /staff/v1/claims/definitions/{id}/{version}` | The wording of one definition at one version, with its hash and the versions that exist. A claim names the definitions its cover is bound to as `id@version`, so an old claim opens the words it was bound to. The portal holds the libraries shipped in `products/definitions/` and any others named in `IC_DEFINITIONS` (comma-separated paths); it refuses to start if two libraries give one ID and version different words. |
| `POST /staff/v1/claims/commands` | `{ "purpose", "partition"?, "command" }`, the command in the claims module's own JSON, with an `Idempotency-Key`. |

`503 claims_unavailable` when the partitions have no leader. `tests/http.rs` runs the console against the claims module's own state machine on two partitions.

- **Investigations.** `cases` lists the restricted cases open to the person signed in (the claims module answers investigators only), each with its claims and the same service billed on other partitions (`elsewhere`).
- **One owner.** The claim view names the owner; where the claims module marks a claimant for communication support it says so in a banner, because a change of owner then needs a handover note and a call.
- **Proof of life is not sent from the app.** Claims takes `ConfirmAlive` from the caller `channels`, but the portal does not send it: it cannot check a liveness result the app reports, so the evidence would be the device's word. A liveness check the app runs is recorded by the identity provider in the customer master, which relays `ConfirmAlive` to every partition (`docs/customers.md`, "Proof of life"). The documents service's repayment letters question is answered 404 here.

## The reinsurance exceptions console

`/reinsurance` shows the reinsurance module's open exceptions (`docs/reinsurance.md`) and what a person can do about each. That module is the source of truth: an exception closes itself once its cause is cured, so there is nothing to acknowledge. The portal reads the module itself, not the event hub the exceptions are also sent to.

| Exception | What a person does |
| --- | --- |
| `notice:<seq>` | Send the notice to claims again (`retry_notice`), or abandon it (`abandon_notice`, two people). |
| `delivery:<seq>` | Abandon the instruction (`abandon_instruction`, two people). The page warns that the core's ledger and the reinsurance ledger then stay apart by its amount. |
| `settlement_mismatch:<n>`, `settlement_returned:<n>` | Record the settlement with its payment reference and the statement's net (`record_settlement`). A returned settlement can instead be offered to payments again under a new reference, with a note of where the decision is filed (`reoffer_settlement`). |
| `facultative_terms_altered:<policy>/<cover>/<layer>/<life>` | Record that the reinsurer was told, and where its answer is filed (`facultative_reinsurer_told`). |
| `reopened_after_reversal:<claim>` | Resume recoveries with a note (`resume_recoveries`) once every disputed item on the claim is resolved; the module refuses it before then (`disputes_open`). |
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


## The payments console

`/payments` is how staff reach the payments module (`docs/payments.md`). That module decides each command by the roles it is told the person holds and keeps them apart itself: whoever prepared a run cannot approve it, and an approver who also maintains payees or connectivity is refused everything (`role_conflict`). The console tells it the signed-in workforce ID and the roles their staff actions give, at New Zealand wall-clock time, under the key `console:{key}`.

| Staff action | Role there |
|---|---|
| `payments.view` | None: reads runs, payments waiting, open exceptions, a party's payees and authorities, reconciliation and readiness. Never a payee's, authority's or refund's account details, and sends nothing. |
| `payments.maker` | `maker`: prepare, fill, check, cancel, close and resubmit runs; take waiting payments; request and release refunds. |
| `payments.approver` | `approver` |
| `payments.payees` | `payee_maintainer`: authorities and payee accounts. |
| `payments.connectivity` | `connectivity_admin`: configuration and redirecting a route to the second bank; the only one who reads the configuration. |
| `payments.legal`, `.reconciler`, `.treasury`, `.collections` | `legal`, `reconciler`, `treasury`, `collections` |
| `payments.compliance` | `compliance`: cases of premium paid by someone other than the owner, and `clear_payer` |

Only people's commands pass (`src/payments.rs`, `COMMANDS`); the banks' answers, screening, the customer's hold as the pump records it, the drill, the core's events, the outbox and pruning are the module's drivers' and are refused here. A held item reads as the customer's own hold (`customer_hold`, or `hold_unconfirmed`, which clears when the maker checks the run again).

| Route | |
|---|---|
| `GET /staff/v1/payments/me` | Your roles there. |
| `GET /staff/v1/payments/{question}` | `open_runs`, `open_exceptions[?account=]`, `matches_to_approve`, `manual_match?exception=`, `party?party=`, `customer_hold?party=`, `waiting`, `run?run=`, `control_totals?run=`, `submission?submission=`, `queued_submissions`, `unconfirmed_submissions`, `authority?authority=`, `payee?payee=`, `refund?refund=`, `awaiting_conversation`, `represent`, `deferred?from=&to=`, `fund_sweep?as_at=`, `fund_cash_settlement?as_at=`, `aged_unidentified_receipts?as_at=`, `reconciliation?account=` and `readiness` (both as of today in New Zealand), `config`, and `payer_cases` and `payer_case?case=` (compliance only). A null answer is 404. |
| `POST /staff/v1/payments/commands` | One command in that module's JSON (`{"type": ...}`), with an `Idempotency-Key`. Its events come back; a refusal is 403 (`not_permitted`, `role_conflict`, `same_person`, ...) or 409 with the module's code. |

## The underwriting console

`/underwriting` is how underwriters reach the underwriting module (`docs/underwriting.md`). That module keeps proposals, cases and decisions and checks each underwriter's authority profile itself (grade, accreditation, limits, conflicts); it takes people's commands only from the caller `underwriting-console` and believes the console about who is acting. So every field that names the person acting (`underwriter`, `by`, `of`, `reviewer`, `booked_by`, `decided_by`) is set here to whoever signed in, whatever the page sent. The underwriter IDs in that module's profiles are workforce directory IDs.

| Staff action | What it opens |
|---|---|
| `underwriting.work` | Queues, proposals, workload, report, exclusions, parked effects; take, hand over, move, ask for evidence and opinions, record an opinion or an interview, propose or countersign a decision, release documents to a reinsurer, send an issue again, put a parked effect back, declare a conflict, read a life's disclosures. |
| `underwriting.senior` | All of that, plus peer review (`draw_peer_review_sample`, `record_peer_finding`) and non-disclosure remedies. |

**Two seniors for a remedy.** `record_non_disclosure_outcome` is prepared by one senior and kept in the replicated channel state (`src/countersign.rs`, ids `underwriting:<key>`), with `decided_by` set to them. A different senior signs it, and only then is it sent, with the signer as `countersigned_by`; the page cannot type it. The preparer can take it back until it is signed; an unreachable module leaves it signed for the signer to finish, and a refusal ends it.

**Reading disclosures** goes through the intake (`POST /v1/disclosures/open`, `{request, proposal, life, underwriter, purpose}`), which has the module allow the reading and record `FileAccessed` before any record is opened. The portal's identity needs the intake's app role `underwriting-console` as well as `new-business`. The request ID is the idempotency key, so a repeat is the same reading.

| Route | |
|---|---|
| `GET /staff/v1/underwriting/me` | Whether you are a senior, the remedies waiting for a second senior, and the business date. |
| `GET /staff/v1/underwriting/{question}` | `proposal?proposal=`, `disclosure_basis?proposal=`, `queue?queue=` (as of now), `workload` (your own), `report?from=&to=`, `exclusions?on=`, `parked`. A null answer is 404. |
| `POST /staff/v1/underwriting/commands` | One command in that module's JSON, with an `Idempotency-Key`. Its events and view come back; a refusal is 403 (`not_authorised`, `not_owner`, ...) or 409 with the module's code. A remedy answers `{prepared}`. |
| `POST /staff/v1/underwriting/sign/{id}`, `.../withdraw/{id}` | Sign and send, or take back, a prepared remedy. |
| `POST /staff/v1/underwriting/disclosures` | `{proposal, life, purpose}`: the intake's answer as it gave it. |

Not sent from here: configuration, the driver's own commands (sweep, acknowledgements, parking), and anything from new business, evidence providers, reinsurance, the issuer or claims.

## The finance console

`/finance` is the staff screen for the finance ledger (`docs/finance.md`): the month-end close dashboard for a period with its checks, waivers and sign-offs, work waiting, manual journals, the trial balance, movements, sources, backdated entries, conduct figures, returns and aged suspense. From it an operator proposes a journal, someone else approves or rejects it, the appointed actuary signs an actuarial journal and the period, the finance lead waives a check with its remediation owner and date, signs off and closes the period, approves a proposed configuration, records a migration account's cut-over total, and retries a stalled feed. An operator or the lead sorts unplaced policies into accounting groups.

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
| `GET /staff/v1/finance/{question}` | One of `status`, `work`, `dashboard`, `checks`, `trial_balance`, `movements`, `sources`, `backdated`, `journal`, `conduct`, `return`, `return_line` (one figure on a filed return and what is behind it), `tax_reconciliation`, `proposals` (charts and close configurations waiting for approval), `aged_suspense`, `extract` (kinds `in_force`, `movement`, `experience`, `claims_by_cause`), `accounting_groups` (counts per group), `accounting_group` (one policy, with `policy` and `partition`; `null` while unplaced), with that question's fields as query parameters. |
| `GET /staff/v1/finance/return_definitions` | The return definitions a return may be filed on, loaded with the finance module's loader from `config/finance/returns/` and any paths in `IC_RETURN_DEFINITIONS`. `file_return` and `restate_return` name one as `id@version` and the portal sends the loaded definition; a definition typed at the console is refused. |
| `POST /staff/v1/finance/commands` | One of that module's commands in its own JSON, with an `Idempotency-Key`. Commands only its own services or an operator's program send (the configuration, proposing a chart or a close configuration, which are reviewed source, the core's event feed, Merkle roots, extracts, conduct facts, and the check and aged item reports other modules make) are refused. |

- **Amounts.** That module counts journal lines in millionths of a minor unit, more than a browser number holds exactly. A line may give `signed_minor` instead (debits positive, credits negative), which the service turns into `signed_fine` exactly, refusing a figure out of range. A cut-over total may likewise come as `total_minor`. A command with no fields, such as `classify_policies`, may be sent as its name alone.
- **Who may.** The ledger checks every role itself and keeps maker and checker apart whatever roles one person holds. A refusal for want of a role comes back as `403 authority`, any other as `409` with its code, and `503 finance_unavailable` when it has no leader.
- **Not on the page.** A chart of accounts, a close configuration and a return definition are proposed from their files through the command route; the page only approves them.

## The group console

`/group` is the staff screen for group scheme administration (`docs/group.md`): the exception queue across every scheme or one, a scheme's standing found by its ID or its policy, a membership file with its preview and refused rows, one member, and a scheme's file layouts. Staff confirm a held file, approve or withdraw one, let backdated changes through or drop them, decide a possible match, put back members removed in error, ask underwriting again for cover above the automatic amount, freeze, unfreeze or resume a scheme, set a mapping, and end a scheme; registering a scheme, amending its terms and agreeing a renewal take the terms as JSON.

The group service holds all of it. The portal sends each command under the caller `group-admin` with the signed-in person as the actor, so its identity needs the group app's `group-admin` role beside `employer-portal` (`deploy/entra/group-app.sh`).

| Staff action | What it allows |
| --- | --- |
| `group.view` | opens the console to read, and to try a mapping (nothing is stored) |
| `group.admin` | the commands as well |

| Route | What it does |
| --- | --- |
| `GET /staff/v1/group/me` | Whether you may open the console, and act there. |
| `GET /staff/v1/group/{question}` | `exceptions` (optional `scheme`), `scheme`, `invoices`, `mapping` (`scheme`), `file` (`scheme`, `file`), `member` (`scheme`, `employee`), `scheme_of` (`policy`). The outbox and the driver's work are not asked from here. |
| `POST /staff/v1/group/commands` | One of `register_scheme`, `amend_terms`, `set_mapping`, `freeze`, `unfreeze`, `resume`, `confirm_held`, `decide_backdating`, `approve_file`, `withdraw_file`, `declare_pool`, `reinstate`, `request_excess_again`, `resolve_match`, `agree_renewal`, `terminate`, in the group module's JSON, with an `Idempotency-Key`. The driver's, a member's and other modules' commands are refused, and a file is received only by upload. |
| `PUT /staff/v1/group/schemes/{policy}/first-admin` | `{ "party" }`. Group administration names a scheme's first administrator, sent to the directory as `group-admin`. Refused (409 `has_administrators`) once the scheme has one: from then on its own administrators manage the list. |
| `POST /staff/v1/group/schemes/{scheme}/mappings/try` | `{mapping, sample}`: what the sample would do read with that mapping. |

- **Refusals.** The module's refusal comes back as `409` with its code, an unknown scheme or file as `404`. If the service refuses the portal itself (its token lacks `group-admin`), that is `502 role_not_held`, the portal's fault and not the person's; an unreachable service is `503`.
- **Tests.** `tests/http.rs` `the_group_console` runs against the group module's own state machine and its own test harness (`tests/common/group_module.rs`), seeded with a possible match.

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
| `IC_GROUP_URL`, `IC_GROUP_SCOPE` | The group service and its app registration's scope, for employers' membership files, invoices and member records. Set together. |
| `IC_PUBLIC_URL` | The https origin people reach the portal at, such as `https://portal.example.co.nz`. Required with Entra sign-in: it names the MCP endpoint in its sign-in discovery document and is the one browser origin the MCP endpoint accepts. |
| `IC_AUTH=dev` | Trust `x-dev-subject` and `x-dev-roles` headers. For a laptop. The service refuses to start with it in Kubernetes. |

**On AKS.** `deploy/k8s/portal.yaml` holds the service account, the settings, the deployment and its network policy; the identity is the `portalClientId` output of `infra/main.bicep`. `deploy/entra/apps.sh workforce` and `apps.sh customers` make these registrations (docs/operations.md, step 4); what they make:

1. In the workforce tenant: an app registration for the portal service, with the app roles `operations` and one per module that calls `/internal/v1`; and a single-page application for the consoles with redirect URIs `https://<host>/staff` and `https://<host>/ops`.
2. In the External ID tenant: an app registration for the customer-facing API, and a single-page application with redirect URIs `https://<host>/adviser` and `https://<host>/employer`. The mobile app has its own registration there.

Fill in the `REPLACE-` values in `portal.yaml`, apply, and route the public host to the `ic-portal` Service through the ingress controller. Put Azure Front Door with its web application firewall in front of the public host.

**On a laptop.** `docker compose -f deploy/compose/docker-compose.yml up --build -d` starts the service at `http://localhost:8090` with development sign-in.

**The pages in a browser.** The pages sign in with the authorization code flow and PKCE and keep the token for the tab only. The content security policy allows this service's own files, each page's inline script by its hash, and calls to this service and the sign-in authorities; nothing else runs and nothing can frame a page. To drive every page in a real browser against a seeded portal:

```sh
cargo test -p ic-channels --test http serve_for_a_browser -- --ignored &
node crates/channels/tests/browser/check.js
```

**Persona demonstration videos.** `crates/channels/tests/browser/record.sh OUT_DIR` starts the seeded portal, walks the same journeys with `VIDEO` set, and writes one captioned MP4 per persona journey with an `index.md` naming the persona (by its ID in `docs/testing/personas/`) and what they do. Because the videos are the browser check, a journey that stops working stops being recorded rather than being shown wrong. Needs ffmpeg. Pages show a service's records as labelled values (`portal.details`), money in dollars, and a page refused outright shows only the refusal.

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
- **Routing follows the submitter.** A request an adviser or support person prepares is routed by their marker, not the customer's who approves it. Asking the master again for the customer at approval would close it.
- **Commission balances are taken to be in one currency** (`IC_PORTAL_CURRENCY`, default NZD), because that module's answers carry none.

### More time for a life who needs it

When a life comes to answer their disclosure or sign a consent, the portal asks the customer master's routing for them. If the adjustments include `more_time` it sends underwriting `set_more_time {proposal, life, more_time: true}` as `underwriting-console` (key `more-time:{proposal}:{life}`), so a marker placed after the proposal opened is picked up. Underwriting extends the dates once. The person is never told why, and a master or module that cannot answer stops nothing.

### What a cover is held at

A `cover` row's body carries `options` where the cover was bought from a menu: each axis with the choice made on it (`{"waiting": "4w", "benefit": "to65"}`), or `{"level": "500"}` for a flat list such as a health excess. It comes from the layer's option code on the read model and is not a health fact, so it goes wherever the row goes: the customer's device, the adviser's book, the staff console. Formula-driven scheme cover has none.

### Pricing a proposed rate table against the book

`POST /staff/v1/finance/table_dry_run` (finance lead or appointed actuary) takes `{as_at, covers, per_life, age_at_start, current, proposed}`, where `current` and `proposed` are the text of the two reviewed Starlark table files. The portal loads both as the product loader does, reads the underwriting loadings for the date from every core partition a page at a time, and asks the ledger's `table_dry_run` with them. Loadings imply health: they pass through for this one answer, are kept nowhere, and the answer carries loaded totals only. If the core cannot be read the run is priced without loadings and the answer has `loadings_read: false`, which the page shows as a warning. The "Rate tables" view of the finance console shows the totals, the movement by rating cell and the covers that move most.

### Acting for someone under an authority

An attorney under an activated enduring power, a guardian, or a person with a written authority that covers viewing signs in as themselves. `GET /app/v1/represented/{party}` lists that person's policies and the authority (kind, basis, scope, dates); `GET /app/v1/represented/{party}/policies/{policy}` is the person's own view of the policy without the rows marked sensitive (`withheld` counts them). Each call asks the customer master's `authority` for the act `view` as the portal, for the person signed in, and the master logs it. No authority in force and no such person both answer 404. `route` (capacity in question) answers 403 `authority_under_review`. A master that cannot answer is 503. The person keeps their own sign-in and their own full view. Acting, as opposed to viewing, is not offered here: a representative's change is entered by staff, who check the authority for that act.

### Someone phones about a policy

`POST /staff/v1/policies/{policy}/caller` with `{caller, case, scope}` tells the member of staff whether to talk to the caller. They must be able to open the policy, and that read is logged as any other. `caller` is the caller's customer reference, or empty when they are not a customer or cannot be identified. An owner is spoken to. Anyone else is spoken to only if the customer master's `authority` answer for the act `view` is `allow` for every owner; `route` (capacity in question) refers the call to legal. Every other case, including a master that cannot answer, gets one set of words that neither confirm nor deny that a policy exists. The contact is a second line in the access log, action `contact.caller_check`, record `policy:{policy}/caller/{reference or unidentified}/{outcome}`. The words are in `CALLER_REFUSED` in `src/http.rs` and are a draft for the conduct team to settle.

### Reviewing vulnerability markers

"Markers to review" on the staff console is for people whose role carries the vulnerability kind (`CareStaff`); anyone else is refused before the master is asked. `GET /staff/v1/care/reviews` lists the master's open `consider_marker` reviews (a marker at its review date, or a prompt after a death or a concern), each with how the person is served now, and the master's lists of kinds and adjustments. `POST /staff/v1/care/reviews/{review}` takes `{party, outcome, kind, review_on, adjustments}` where the outcome is `renew`, `change`, `remove` or `no_marker`. The portal calls the master as `staff` under the purpose `vulnerability_support` with the member of staff as actor, changes the marker first and closes the prompt after, so a refusal leaves the prompt open. A marker has no free text, and the recorded reason is the outcome and the review ID only.

## Staff work in other services

Some work is done by another service, and the portal is where people reach it. The portal asks the four questions (`StaffAct`: the action, the record, who it is about and the case), writes the access log line whether or not it is allowed, and only then sends the work on. Nobody works their own record or that of someone they are connected to. The other service decides again by its own rules.

| Route | Action | Sent to |
| --- | --- | --- |
| `POST /staff/v1/new-business/quote` | `new_business.quote` | The core's dry run on the group's partition. Nothing is written. |
| `GET /staff/v1/core/policies/{number}` | `policy.core_view` (insurance-sensitive class) | Every partition's read side. A policy that is not there and one the person may not see are the same refusal. |
| `POST /staff/v1/customers/search`, `/match`, `POST /staff/v1/customers` | `customers.find`, `customers.create` | The customer master as `staff`, purpose `servicing`. |
| `GET /staff/v1/customers/{party}` (with `as_of`, `known_at`), `/history`, `/relationships`, `/due-diligence` | `customers.view` | The same. |
| `POST /staff/v1/customers/{party}/commands` | `customers.change` | The same, with the idempotency key. |
| `POST /staff/v1/customers/merges`, `/unmerges`, `GET /staff/v1/customers/reviews`, `POST /staff/v1/customers/reviews/{review}/dismiss` | `customers.merge` | As `staff`, purpose `duplicates`. |
| `GET /staff/v1/customers/{party}/export` | `customers.export` | As `staff`, purpose `subject_access`. |
| `GET /staff/v1/customers/{party}/audit`, `GET /staff/v1/actors/{actor}/actions` | `customers.audit` | As `privacy`. |
| `GET /ops/v1/reconciliation` | operations role | Every partition's end-of-day view, joined: breaks, in-transit, unfinished policies. |
| `GET /ops/v1/customers/index`, `POST /ops/v1/customers/{party}/verify`, `/rewrap`, `/reshred` | operations role | The customer master as `operations`. |

The master is always told who the member of staff is (`x-ic-actor`) and never names a party to act for. For this the portal's identity holds the `staff`, `privacy` and `operations` roles on the customer master (docs/operations.md, step 4).

A policy is not issued here. The core trusts its new business caller to send only terms underwriting accepted, so a policy is issued from underwriting's decision (the path the portal's applications already take), never from an application typed in by hand. Policy servicing changes are not here either: staff enter them for the customer (`POST /staff/v1/requests`) and the customer approves them in the app, as before. The end-of-day close runs on its schedule as a workflow and is not started from the portal; its result is what `/ops/v1/reconciliation` reads.

## The MCP server

`POST /mcp` is a Model Context Protocol server (Streamable HTTP, JSON answers, protocol versions 2025-11-25, 2025-06-18 and 2025-03-26). It lets an AI assistant do anything the person could do in the portal, and nothing more.

**One tool per function.** Every route the portal serves is a tool, except the probes, the pages and their files, device enrolment and challenges, the sync stream and `/internal/v1` (the list is `mcp::EXCLUDED`, each with its reason). A test reads the router and fails the build when a route is neither a tool nor excluded. A tool call is sent through the portal's own router with the caller's own token, so it passes exactly the checks the screens do: the four questions for staff, relationships for customers, advisers and employers, the access log, idempotency. A write is given the idempotency key the caller passes (`idempotency_key`), or a fresh one. Path parameters with a fixed set of values (module questions, commands) are listed as such in the tool's input schema. Files go both ways whole: an upload is `content_base64`, a PDF or extract comes back as an embedded resource.

**Who sees which tools.** `tools/list` shows only what the caller's sign-in reaches: the public tools always; the customer, adviser and employer tools with an External ID token bound to a person; the staff tools with a workforce token bound to a member of staff; the operations tools with the `operations` role. A refusal by a tool is a tool result with `isError` and the service's own reason (`Refused (403 not_authorised): ...`), so the assistant can explain it.

**Sign-in.** The endpoint takes the same bearer tokens as the screens. Without one it answers 401 with `WWW-Authenticate: Bearer resource_metadata=".../.well-known/oauth-protected-resource/mcp"`; that document (RFC 9728) names both tenants as authorization servers and the scopes to ask for, so an MCP client signs the person in itself. Register the client's redirect URI on the staff consoles' or the partner portals' single-page application, as for any other client. A browser may call the endpoint only from `IC_PUBLIC_URL`'s origin.

**What it cannot do.** Anything that needs the customer's own phone to sign (an approval, a payment instruction) can be prepared over MCP but is still signed on the phone, because the proof is made by the device's hardware key. `GET /mcp` (a server-sent event stream) is not offered: every answer is a single JSON reply.

## How the screens are built

Every screen uses one design system: tokens, light and dark themes, three page frames (staff consoles with a navigation rail, partner portals, public pages), views as tabs, and shared components. It is described in [design-system.md](design-system.md). The browser check (`tests/browser/check.js`) takes screenshots of every page with `SHOTS=dir`, in the dark theme with `THEME=dark` and at phone width with `WIDTH=390`.
