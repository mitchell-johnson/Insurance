# Customer master

`crates/customers` (`ic-customers`) is the one place the insurer knows who a person or organisation is. The core, claims, group schemes, commission and documents know people only by an opaque party ID (`pty_…`) and ask here for anything about them. It is a service of its own on PostgreSQL, not a module on the ledger: personal data must be erasable, and the ledger is append-only.

## What it holds

- **Parties.** A person or an organisation, with a lifecycle: prospect, active, deceased, exited. Each has a group (`grp_…`) used to keep related parties on one core partition.
- **Facts on two timelines.** Every value is a statement with `valid_from` (and an optional `valid_to`) and the time it was recorded. A read answers "what was true on `as_of`, as we knew it at `known_at`"; both default to now. Among the statements covering a date, the one recorded last wins, so a backdated correction takes effect without rewriting history. A correction the business refuses is kept as a declined correction, visible to the person.
- **Relationships**, each with a start and an optional end: to a policy (`owner`, `life_insured`, `beneficiary`, `dependant`, `payer`, `interested_party`, `assignee`, `claimant`, `adviser`), to a scheme (`employer_administrator`, `scheme_member`, `broker`), and to another party (`director`, `trustee`, `signatory`, `beneficial_owner`, `employee`, `attorney`, `guardian`, `executor`, `partner`, `parent`, `staff`).
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
| `POST /v1/parties/{id}/commands` | by policy | `record`, `decline_correction`, `add_relationship`, `end_relationship`, `set_consent`, `record_verification`, `set_risk_rating`, `activate`, `mark_deceased`, `retract_death`, `exit`, `assign_group`, `place_hold`, `release_hold`, `record_concern`. `If-Match` carries the version; `Idempotency-Key` makes a retry safe. A command that changes nothing writes nothing. |
| `GET /v1/parties/{id}/contact-points?purpose=notices&channel=email\|sms\|post\|in_app\|push&relationship=` | documents | The documents contract: `{name, destination, language, format, deceased}`, or 404 when there is no contact point on that channel. |
| `GET /v1/parties/{id}/relationships`, `GET /v1/relationships?target=policy:P1` | by policy | Relationships of a party, or to a policy, scheme or party. |
| `GET /v1/parties/{id}/routing` | portal, staff, claims, servicing, complaints | `{route_to_person, adjustments, review_on}`, and `kind` for `vulnerability_support`. |
| `GET /v1/parties/{id}/authority?actor=&act=&on=&benefits_actor=` | portal, staff, claims, servicing, payments, complaints | May a representative do `act` for this party on `on`? `{decision: allow \| refuse \| route, reason, relationship}`. Representatives are `attorney`, `guardian` and `authorised_representative` relationships held by the actor to `party:<id>`, each with `basis` (enduring_power_of_attorney, court_order, written_authority), `document` (the vault reference) and `scope` (view, pay_premiums, change_payment_method, change_contact_details, change_bank_details, change_cover, change_beneficiary, surrender, claim, complain, all); `valid_from` is activation, `valid_to` the end. An act in scope that benefits the representative (beneficiary, bank details, surrender, claim) is routed to a person. Every answer is audited with the actor. |
| `GET /v1/parties/{id}/export?purpose=subject_access` | staff; portal for the person themselves | Every record of the party and those merged into it (Privacy Act access request). |
| `GET /v1/parties/{id}/audit` | privacy | Who saw what. |
| `POST /v1/merges`, `POST /v1/unmerges`, `GET /v1/reviews`, `POST /v1/reviews/{id}/dismiss` | staff, migration | Duplicates (`kind: duplicate`): a merge is an event on both parties; the absorbed ID resolves to the survivor until the merge is undone. Prompts (`kind: consider_marker`, with `reason`): opened by `record_concern`, a third party's worry (`from` family, friend, carer, professional, staff, other; `about` financial_abuse, capacity, wellbeing, bereavement, coercion, other); a trained person decides on a marker and dismisses the prompt. Nothing about the person is told to whoever raised it. |
| `POST /v1/admin/verify/{id}`, `/rewrap`, `/reshred` | operations | Chain check, key rotation, erasing again after a restore. |
| `GET /healthz`, `GET /readyz` | probes | Readiness fails while the database does not answer. |

Errors are `{"error": {"code", "message"}}`. 503 always means try again; it is never used for an answer.

## Deaths to claims

Every death recorded here, whoever recorded it, is told to claims: `RecordDeath { party, date_of_death }` from caller `customer-master`, proposed to every core partition, because a person's claims can sit on any of them and each partition finds its own. A row in `death_relay` is written in the same transaction as the death and removed once every partition has accepted it. A refusal (for example before the core has opened a business date) or an unreachable cluster keeps it, with the reason and an attempt count, for the next pass. Each attempt uses its own key (`death:{party}:{date}:{partition}:{attempt}`), because the core remembers a refusal under its key; claims treats the same death again as nothing new. A retraction goes the same way, as `RetractDeath` for the date that was recorded (keys `retraction:…`), and replaces a death on that date not yet told; claims restores the benefit or sends it to review. Without `IC_CLUSTER_NODES` deaths are kept and not sent.

## Retention and erasure

The schedule is code (`src/retention.rs`, version `SCHEDULE_VERSION`): each field class is kept seven years after the last relationship ends or the last change, verification evidence five, prospects with no relationship two. A legal hold or an unresolved merge stops erasure.

The sweep runs in every replica (hourly by default) over parties unchanged for 30 days. It writes the batch to the erasure register first, then for each party takes its row lock, checks again, destroys the wrapped key, blanks every payload, deletes its blind indexes and appends a `shredded` tombstone. The chain stays verifiable; the content is gone. Absorbed parties go with their survivor.

**Restoring a backup.** A restore brings erased keys back. The register (`customers-erasures`, write-once blob storage kept longer than any backup) lists every erasure. After any restore, before the service takes traffic, an operator calls `POST /v1/admin/reshred` with the date the backup was taken; every party listed since is erased again.

## Running it

The binary is `ic-customers`; settings come from the environment.

| Setting | Meaning | Default |
|---|---|---|
| `IC_CUSTOMERS_PG_HOST`, `IC_CUSTOMERS_PG_USER`, `IC_CUSTOMERS_PG_DATABASE` | Azure PostgreSQL, Entra sign-in as the managed identity, TLS required | database `customers` |
| `IC_CUSTOMERS_DATABASE_URL` | Instead of the above, a plain connection string; refused unless the host is this machine | |
| `IC_CUSTOMERS_POOL_SIZE` | Connections per replica | 16 |
| `IC_CUSTOMERS_KEY_VAULT`, `IC_CUSTOMERS_KEK` | Key Vault URL and wrapping key | key `customers-kek` |
| `IC_CUSTOMERS_INDEX_KEY_FILE` (or `IC_CUSTOMERS_INDEX_KEY`) | Blind-index key, 32 characters or more | required |
| `IC_CUSTOMERS_TENANT`, `IC_CUSTOMERS_AUDIENCE`, `IC_CUSTOMERS_AUTHORITY` | Entra ID token checks | Entra public cloud |
| `IC_EVENT_HUBS_NAMESPACE`, `IC_CUSTOMERS_EVENT_HUB`, `IC_CUSTOMERS_PRIORITY_HUB`, `IC_CUSTOMERS_HUB_PARTITIONS` | Where events go | `customer-events`, `customer-priority`, 8 |
| `IC_CUSTOMERS_REGISTER_ENDPOINT`, `IC_CUSTOMERS_REGISTER_CONTAINER` | Erasure register | container `customers-erasures` |
| `IC_CUSTOMERS_LISTEN`, `IC_CUSTOMERS_CYCLE_SECONDS`, `IC_CUSTOMERS_SWEEP_MINUTES` | Port and background pace | `0.0.0.0:8080`, 2, 60 |
| `IC_CLUSTER_NODES`, `IC_CLUSTER_TOKEN_FILE` | The ledger cluster, for telling claims of deaths | optional; deaths wait without it |

Azure resources are in `infra/customers.bicep` (its own PostgreSQL Flexible Server, the wrapping key, the index-key secret, the hubs, the register and the identity). The Kubernetes manifests are `deploy/k8s/customers.yaml`. The service applies its own migrations at start under an advisory lock, so replicas starting together are safe. SIGTERM stops it cleanly.

Tests need a PostgreSQL server named by `IC_TEST_POSTGRES_URL` (CI starts one); each test makes and drops its own database. Without it the database tests skip.

## Not built yet

- **Relationship changes to the channels directory.** The channels module expects `BindSubject`, `RecordOwnership`, `RecordAssignment` and `RecordDependant` from the customer master. The events are on `customer-events`; the relay that turns them into those commands is not written.
- **Changing the index key** needs a rebuild of every blind index from the decrypted facts; there is no command for it yet.
- **Locking the register's immutability policy** in prod is a deliberate, irreversible step for a person once the period is agreed.
