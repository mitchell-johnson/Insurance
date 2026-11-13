# Customer master

`crates/customers` (`ic-customers`) is the one place the insurer knows who a person or organisation is. The core, claims, group schemes, commission and documents know people only by an opaque party ID (`pty_…`) and ask here for anything about them. It is a service of its own on PostgreSQL, not a module on the ledger: personal data must be erasable, and the ledger is append-only.

## What it holds

- **Parties.** A person or an organisation, with a lifecycle: prospect, active, deceased, exited. Each has a group (`grp_…`) used to keep related parties on one core partition.
- **Facts on two timelines.** Every value is a statement with `valid_from` (and an optional `valid_to`) and the time it was recorded. A read answers "what was true on `as_of`, as we knew it at `known_at`"; both default to now. Among the statements covering a date, the one recorded last wins, so a backdated correction takes effect without rewriting history. A correction the business refuses is kept as a declined correction, visible to the person.
- **Relationships**, each with a start and an optional end: to a policy (`owner`, `life_insured`, `beneficiary`, `dependant`, `payer`, `interested_party`, `assignee`, `claimant`, `adviser`), to a scheme (`employer_administrator`, `scheme_member`, `broker`), and to another party (`director`, `trustee`, `signatory`, `beneficial_owner`, `employee`, `attorney`, `guardian`, `executor`, `partner`, `parent`, `staff`).
- **Gender** as the person tells it, in any words (up to 100 characters), identity class. It is never a rating factor and never matched; the rating sex factor belongs to underwriting and is kept apart (decision 1 in `docs/decisions.md`).
- **Consents** by purpose and date, **identity verifications**, **risk ratings**, **legal holds**, and a **vulnerability marker**: `{kind, review_on, adjustments}` from fixed lists (`validate.rs`, `MARKER_KINDS`, `ADJUSTMENTS`), so no diagnosis or free text can be recorded; `false` removes it. The routing answer gives anyone serving the person `route_to_person`, the adjustments and the review date; the kind only to staff reading for `vulnerability_support`. The person sees their own marker in the app. When `review_on` comes, the hourly sweep opens one `consider_marker` prompt (reason `review_date`); only the date is kept outside the sealed events, in `marker_reviews`. Any other read leaves it out without a trace, not even as a withheld field.

Fields, their classes and normal forms are in `src/model.rs` and `src/validate.rs`. IRD numbers are checked against their check digit; email and phone numbers are normalised; names keep their spelling and gain a folded key for matching.

## How it is stored

Each party is a hash-chained stream of events (`party_events`). The current state (`parties`) is a projection that can always be rebuilt from the stream.

- **Per-party keys.** Every event payload is sealed with AES-256-GCM under the party's own data key, bound to the party ID and sequence number. The data key is wrapped by `customers-kek` in Key Vault (RSA-OAEP-256) and cached briefly in memory. `POST /v1/admin/rewrap` moves every wrapped key to the current key version after a rotation.
- **Tamper evidence.** Each event carries a salted commitment to its plaintext and the hash of the one before it. `POST /v1/admin/verify/{id}` re-checks a party's chain. A trigger in the database forbids deleting events or changing anything but blanking a ciphertext.
- **Lookups without plaintext.** Matching and search use HMAC blind indexes (IRD number, email, mobile, family name with birth date, given name with birth date, phonetic names, registration number, organisation name) under the index key, which lives in Key Vault and is mounted as a file.
- **Audit.** Every read, refusal and change is written to `audit` (append-only by trigger) with the caller, the person acting, the purpose, the fields released and the policy version. A read that cannot be logged is not served.
- **Outbox.** Changes go out through `outbox`, published to Event Hubs at least once: `customer-events` for everything, `customer-priority` for changes that take access away (deaths, holds, merges, erasures). A party's events always land on the same partition. Events carry IDs, kinds and field names, never values.

## Access

Callers present an Entra ID token for the customer master's application. The app role names the calling service; a token with several roles names one with `x-ic-caller`. The caller also sends `x-ic-purpose`, and may send `x-ic-actor` (the person at the keyboard) and `x-ic-party` (the party that person is, for self-service).

Decisions are made by the Cedar policy in `config/cedar/customers.cedar`: which service may read which fields for which purpose, which commands each may send, and relationship-scoped reads (an adviser sees their clients, an employer administrator sees their scheme's members' names and birth dates). Fields a caller may not see are masked where a masked form exists, otherwise withheld. Break-glass reads are allowed for everything but the restricted class, alerted and marked in the audit.

## The API

| Route | Who | What |
|---|---|---|
| `POST /v1/parties/match` | group schemes, claims, migration | The group service's contract. Body: `source`, `given_name`, `family_name`, `date_of_birth`, `email`, `mobile_phone`, `national_id` (IRD), `create_if_none`. Answers `{"outcome": "linked", "party"}`, `{"outcome": "created", "party"}`, `{"outcome": "possible", "candidates"}` or `{"outcome": "none"}`, with `ignored` naming values set aside as malformed. Linking needs a strong identifier (IRD, email or mobile) and a score of 80; 40 to 80 is possible and goes to a person. A retried request returns the same answer. |
| `POST /v1/parties` | staff, group, servicing, complaints | Create. Refused with 409 `possible_duplicate` and the candidates unless `allow_duplicate`, which opens a review. `Idempotency-Key` makes a retry safe. |
| `GET /v1/parties/{id}` | by policy | Facts for `?purpose=`, `as_of`, `known_at`. An absorbed ID answers for its survivor. 410 once erased. |
| `GET /v1/parties/{id}/history` | by policy | Every statement released to the caller. |
| `POST /v1/parties/{id}/commands` | by policy | `record`, `decline_correction`, `add_relationship`, `end_relationship`, `set_consent`, `record_verification`, `set_risk_rating`, `activate`, `mark_deceased`, `retract_death`, `exit`, `assign_group`, `place_hold`, `release_hold`, `record_concern`, `question_capacity`, `settle_capacity`, `place_spending_hold`, `request_hold_removal`, `cancel_hold_removal`, `override_spending_hold`. `If-Match` carries the version; `Idempotency-Key` makes a retry safe. A command that changes nothing writes nothing. |
| `GET /v1/parties/{id}/contact-points?purpose=notices&channel=email\|sms\|post\|in_app\|push&relationship=` | documents | The documents contract: `{name, destination, language, format, deceased}`, or 404 when there is no contact point on that channel. With `relationship=` (policyholder, claimant, member and so on), a contact point the person chose for that relationship and channel (the contact field `relationship_contacts`, a list of `{relationship, channel, destination}`, one per relationship and channel) is answered first; otherwise their own. |
| `GET /v1/parties/{id}/relationships`, `GET /v1/relationships?target=policy:P1` | by policy | Relationships of a party, or to a policy, scheme or party. |
| `GET /v1/parties/{id}/relationships/{relationship}` | by policy (as above) | One relationship by its reference, the holding party and the relationship's id: `{party, id, kind, target, valid_from, valid_to, attributes, in_force}`, 404 if unknown. Payments checks a payer's reference this way before clearing a third-party payer case (FIN-015, FIN-016). A payer who pays for someone else with no other link is recorded as `premium_payer` from the payer to the owner (`target: party:<owner>`), with a `basis` attribute such as employer or parent; an existing `parent` or `director` relationship links them just as well. |
| `GET /v1/parties/{id}/routing` | portal, staff, claims, servicing, complaints | `{route_to_person, adjustments, review_on}`, and `kind` for `vulnerability_support`. With `?act=` (a representative scope) for a value-moving act (surrender, beneficiary, bank details, payment method, cover) on a marked person, it also answers `pause_for_conversation` (the person asked for a pause, or a risk of financial abuse) and `specialist_review` (a risk of financial abuse), never why; neither is a refusal. |
| `GET /v1/parties/{id}/due-diligence` | new-business, underwriting, policy-servicing, staff, compliance, payments | Whether due diligence is complete before the party owns a policy: `{complete, open, to_specialist, connected}`. Open items are `identity` and `screening` (the latest `record_verification` of each kind has not passed; `screening` is the method `screening`, any other method is an identity check, remote ones included), `..._referred` when the latest went to a specialist, and `risk_rating` when none is current. For an organisation, every trustee, director and beneficial owner in force today is checked too and listed under `connected`, with their items as `trustee:<party>:<item>`; those listed are the people who may act for it today. A signing rule goes on the ownership relationship as the attribute `signing_rule`. |
| `GET /v1/parties/{id}/spending-hold` | payments, policy-servicing, claims, staff, legal, core; the person in the app | The person's own hold on money going out: `{held, placed_at, removal: {requested_at, ends_at} \| null}`. See "A person's own spending hold". |
| `GET /v1/actors/{actor}/actions` | privacy | Everything one person did as the actor, and every authority question asked about them, across all parties, dated and newest first: the record a guardian or attorney gives the court or the family. |
| `GET /v1/parties/{id}/authority?actor=&act=&on=&benefits_actor=` | portal, staff, claims, servicing, payments, complaints | May a representative do `act` for this party on `on`? `{decision: allow \| refuse \| route, reason, relationship}`. Representatives are `attorney`, `guardian` and `authorised_representative` relationships held by the actor to `party:<id>`, each with `basis` (enduring_power_of_attorney, court_order, written_authority), `document` (the vault reference) and `scope` (view, pay_premiums, change_payment_method, change_contact_details, change_bank_details, change_cover, change_beneficiary, surrender, claim, complain, all); `valid_from` is activation, `valid_to` the end. An act in scope that benefits the representative (beneficiary, bank details, surrender, claim) is routed to a person. While the person's capacity is in question (`question_capacity` by staff, claims or servicing; `settle_capacity` with `has_capacity` or `lacks_capacity` by legal only) every act of a named representative, activated or not, is routed (`capacity_in_question`); nothing else about the record changes, so cover, claims and notices go on. Every answer is audited with the actor. |
| `GET /v1/parties/{id}/export?purpose=subject_access` | staff; portal for the person themselves | Every record of the party and those merged into it (Privacy Act access request). |
| `GET /v1/parties/{id}/audit` | privacy | Who saw what. |
| `POST /v1/merges`, `POST /v1/unmerges`, `GET /v1/reviews`, `POST /v1/reviews/{id}/dismiss` | staff, migration | Duplicates (`kind: duplicate`): a merge is an event on both parties; the absorbed ID resolves to the survivor until the merge is undone. Prompts (`kind: consider_marker`, with `reason`): opened by a death for the other owners of a policy the person owned and a recorded partner (reason `bereavement`, one open at a time), or by `record_concern`, a third party's worry (`from` family, friend, carer, professional, staff, other; `about` financial_abuse, capacity, wellbeing, bereavement, coercion, other); a trained person decides on a marker and dismisses the prompt. Nothing about the person is told to whoever raised it. |
| `POST /v1/admin/verify/{id}`, `/rewrap`, `/reshred` | operations | Chain check, key rotation, erasing again after a restore. |
| `GET /healthz`, `GET /readyz` | probes | Readiness fails while the database does not answer. |

Errors are `{"error": {"code", "message"}}`. 503 always means try again; it is never used for an answer.

## Deaths to claims

Every death recorded here, whoever recorded it, is told to claims: `RecordDeath { party, date_of_death }` from caller `customer-master`, proposed to every core partition, because a person's claims can sit on any of them and each partition finds its own. A row in `death_relay` is written in the same transaction as the death and removed once every partition has accepted it. A refusal (for example before the core has opened a business date) or an unreachable cluster keeps it, with the reason and an attempt count, for the next pass. Each attempt uses its own key (`death:{party}:{date}:{partition}:{attempt}`), because the core remembers a refusal under its key; claims treats the same death again as nothing new. A retraction goes the same way, as `RetractDeath` for the date that was recorded (keys `retraction:…`), and replaces a death on that date not yet told; claims restores the benefit or sends it to review. Without `IC_CLUSTER_NODES` deaths are kept and not sent.

**Proof of life.** A passed identity check by a method that sees the living person (`document_and_selfie`, `liveness`, `in_person`, `video_call`), made within 30 days of being recorded, of someone not recorded as dead, is told to claims as `ConfirmAlive { party, source: identity_<method>, evidence_ref }` on every partition. Claims completes any open proof of life on that person's benefits in payment. Rows wait in `alive_relay` with the reason until every partition accepts (keys `alive:{row}:{partition}:{attempt}`). A database check, or a death register search with no match, proves nothing and is not sent.

Relationship changes on a policy go to the channels directory, which decides what the portal and app show to whom (`docs/channels.md`). An owner added becomes `RecordOwnership` and an owner ended `EndOwnership` (the directory removes them only while they are still the recorded owner, so the order against the next owner does not matter), an assignee added or ended becomes `RecordAssignment` (active or not), and a dependant with `controls_from` becomes `RecordDependant` and a dependant ended `EndDependant`. They are proposed to the `channels` module through the same cluster, as `Actor::Module("customer-master")`. A row in `directory_relay` is written in the same transaction as the change and removed once the directory accepts or refuses it for good (a refusal such as `unknown_policy` is recorded on the row with its reason). An unreachable cluster keeps the row; while one row for a party waits, that party's later rows wait behind it, so the directory sees one person's changes in order. Keys are `directory:{row}:{attempt}`. Sign-in bindings (`BindSubject`) come from the identity service, not from here.

## Retention and erasure

The schedule is code (`src/retention.rs`, version `SCHEDULE_VERSION`): each field class is kept seven years after the last relationship ends or the last change, verification evidence five, prospects with no relationship two. A legal hold or an unresolved merge stops erasure.

### A person's own spending hold

Decision 3 in `docs/decisions.md` (persona row SVC-022). A person can put a hold on their own money going out: refunds, surrenders, withdrawals, policy loans and changes to their payout account. Premium collection and claim payments to payees they already nominated are not held; payments and the core enforce it, from the events (`spending_hold_placed` goes on the priority hub) and from `GET /v1/parties/{id}/spending-hold`.

- `place_spending_hold` (the person in the app, or staff when the person asks): takes effect at once. Sent again while a removal waits, it cancels the removal.
- `request_hold_removal {verification}` (the person in the app only): names a passed identity check (`evidence_ref` of a `record_verification` from the identity service) made after the hold was placed and within the last 24 hours. The hold stands for the cooling-off period (`IC_CUSTOMERS_HOLD_COOLING_OFF_DAYS`, default 7) and then the hourly sweep ends it (`spending_hold_ended`, reason `cooling_off`).
- `cancel_hold_removal` (the person): the hold stays.
- `override_spending_hold {reference}` (legal only): a court order or legal direction ends it at once, recorded with its reference (reason `legal_direction`).
- Staff cannot ask for it to end, and no caller can send the cooling-off ending (`end_spending_hold` is forbidden in the policy). Only the request time is kept outside the sealed events, in `spending_hold_removals`.
- **Telling the core.** The core refuses a withdrawal, a surrender or a credit refund while a policy is held, so the master tells it policy by policy: `SetOwnerHold { policy, held, reference }` from caller `customer-master`. When a hold is placed or ends, every policy the person owns (an `owner` relationship to the policy that has not ended) gets one; while it stands a new ownership is held at once and an ownership that ends is released on its end date (also queued when the hold is placed), so the next owner is not held. References are `spending-hold:{party}:{seq}`, `spending-hold-ended:{party}:{seq}`, `ownership-ended:{party}:{seq}`, or legal's reference when legal lifts it. Rows wait in `owner_hold_relay` (written in the same transaction as the event, with the day they are due) and the worker proposes each to every partition, since the master does not know a policy's group; `unknown_policy` means "not here". A row is removed once a partition takes it. A refusal or an unreachable cluster keeps it with the reason and holds up that person's later rows, so the core never sees a hold end before it starts. A row no partition takes (a policy still being issued) is tried each pass for 7 days, then given up with a warning. Payments' check at the bank stays as the second line.

### Changing the index key

Every blind-index entry carries the public id of the key it was made under (the first eight bytes of a MAC under the key, in hex), and `index_state` records which parties have been indexed under which key. To change the key:

1. Put the new key in Key Vault as a second secret and mount it as `IC_CUSTOMERS_NEXT_INDEX_KEY_FILE`. From then on every write indexes under both keys, and the background pass opens up to 200 parties a cycle and indexes them under both. Lookups still use the current key only.
2. Watch `GET /v1/admin/index` (app role `operations`) until `ready_to_promote` is true (`waiting_next` is 0).
3. Make the new key `IC_CUSTOMERS_INDEX_KEY_FILE` and remove the next key. Lookups now use only the new key's entries.
4. The background pass removes entries under keys no longer in use, 5000 a cycle, but only once the current key covers everyone; `retired_entries` reaches 0. Then delete the old secret.

Promoting early loses nothing: anyone the new key has not covered is indexed by the next passes, and old entries stay until then, though such a person is not found by a lookup until they are. Entries made before keys were named are adopted as the current key's once, after one party's entries are checked against it; if they do not match, everyone is indexed afresh.

The sweep runs in every replica (hourly by default) over parties unchanged for 30 days. It writes the batch to the erasure register first, then for each party takes its row lock, checks again, destroys the wrapped key, blanks every payload, deletes its blind indexes and appends a `shredded` tombstone. The chain stays verifiable; the content is gone. Absorbed parties go with their survivor.

**Restoring a backup.** A restore brings erased keys back. The register (`customers-erasures`, write-once blob storage kept longer than any backup) lists every erasure. After any restore, before the service takes traffic, an operator calls `POST /v1/admin/reshred` with the date the backup was taken; every party listed since is erased again.

**Locking the register.** The register is kept for 10 years (agreed with Mitchell on 2026-11-13). Bicep sets the period but cannot lock it, so once, in prod, after the first deployment of `infra/customers.bicep`, run `tools/lock-erasure-register.sh <resource-group> <registerAccountName output>`. It checks the policy is the agreed period and unlocked, asks for the account name, then locks it. From then on the period can only be lengthened and nobody can delete a page early. Leave it unlocked in dev and test, where accounts are torn down.

## Running it

The binary is `ic-customers`; settings come from the environment.

| Setting | Meaning | Default |
|---|---|---|
| `IC_CUSTOMERS_PG_HOST`, `IC_CUSTOMERS_PG_USER`, `IC_CUSTOMERS_PG_DATABASE` | Azure PostgreSQL, Entra sign-in as the managed identity, TLS required | database `customers` |
| `IC_CUSTOMERS_DATABASE_URL` | Instead of the above, a plain connection string; refused unless the host is this machine | |
| `IC_CUSTOMERS_POOL_SIZE` | Connections per replica | 16 |
| `IC_CUSTOMERS_KEY_VAULT`, `IC_CUSTOMERS_KEK` | Key Vault URL and wrapping key | key `customers-kek` |
| `IC_CUSTOMERS_INDEX_KEY_FILE` (or `IC_CUSTOMERS_INDEX_KEY`) | Blind-index key, 32 characters or more | required |
| `IC_CUSTOMERS_HOLD_COOLING_OFF_DAYS` | How long a spending hold stands after the person asks for it to end | 7 |
| `IC_CUSTOMERS_NEXT_INDEX_KEY_FILE` (or `IC_CUSTOMERS_NEXT_INDEX_KEY`) | The key being changed to, while a change is under way | optional; must differ from the current key |
| `IC_CUSTOMERS_TENANT`, `IC_CUSTOMERS_AUDIENCE`, `IC_CUSTOMERS_AUTHORITY` | Entra ID token checks | Entra public cloud |
| `IC_EVENT_HUBS_NAMESPACE`, `IC_CUSTOMERS_EVENT_HUB`, `IC_CUSTOMERS_PRIORITY_HUB`, `IC_CUSTOMERS_HUB_PARTITIONS` | Where events go | `customer-events`, `customer-priority`, 8 |
| `IC_CUSTOMERS_REGISTER_ENDPOINT`, `IC_CUSTOMERS_REGISTER_CONTAINER` | Erasure register | container `customers-erasures` |
| `IC_CUSTOMERS_LISTEN`, `IC_CUSTOMERS_CYCLE_SECONDS`, `IC_CUSTOMERS_SWEEP_MINUTES` | Port and background pace | `0.0.0.0:8080`, 2, 60 |
| `IC_CLUSTER_NODES`, `IC_CLUSTER_TOKEN_FILE` | The ledger cluster, for telling claims of deaths and the channels directory of relationships | optional; both wait without it |

Azure resources are in `infra/customers.bicep` (its own PostgreSQL Flexible Server, the wrapping key, the index-key secret, the hubs, the register and the identity). The Kubernetes manifests are `deploy/k8s/customers.yaml`. The service applies its own migrations at start under an advisory lock, so replicas starting together are safe. SIGTERM stops it cleanly.

Tests need a PostgreSQL server named by `IC_TEST_POSTGRES_URL` (CI starts one); each test makes and drops its own database. Without it the database tests skip.

## Not built yet

