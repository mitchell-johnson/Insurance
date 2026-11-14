# Documents and correspondence

`crates/documents` (`ic-documents`) issues every document the insurer sends, keeps each exactly as issued, delivers it, and proves delivery where the law or the design requires proof. This page says how it works, how to run it, and what is not built.

## The parts

| Part | File | What it does |
|---|---|---|
| State machine | `state.rs`, `command.rs`, `event.rs`, `model.rs` | Decides everything. Deterministic, replicated in its own group on the ledger nodes as module `documents`. Holds no personal data. |
| Templates | `template.rs`, `render.rs`, `kinds.rs` | Wording as versioned configuration, and the renderer. |
| Module host | `module.rs` | The questions the module answers, and how core events reach it. |
| Vault | `crypto.rs`, `vault.rs` | Every document encrypted under its own key in Azure Blob Storage. |
| Channels | `channels.rs` | Email and SMS through Azure Communication Services, post through a print outbox, the contact directory, provider delivery reports. |
| Driver | `driver.rs` | Renders, stores, sends, reports, publishes and deletes. Stateless and safe to stop anywhere. |

## Templates

A template is configuration: an id, a version, a kind, one body per language, and the shared parts it is composed from. It is identified by a hash of its content and its parts. A version is never changed; a correction is a new version.

A version becomes active only after:

1. it validates (every field it uses is known, every translation uses the same fields and says which language controls, the wording reads at or below the configured reading grade);
2. legal and product have approved it, and compliance as well for anything that must be proven delivered;
3. a rendering run has passed: synthetic personas rendered and compared with the expected hash, covering every product version the template is bound to and every language. A failed run is recorded and blocks activation.

Nobody approves their own template. `DisableTemplate` stops a version being used at once when it is found to be wrong.


**Where a clause is used.** `Query::PartUsage { part }` (`DocumentsState::part_usage`) lists every template version that is not disabled and uses a shared part, the product versions their documents serve, and `behind`: the products whose newest template still uses an older version of the part than the newest registered. A change to a clause is one `RegisterPart`; the register then shows every product it has still to reach.
**Placeholder wording** (`placeholders.rs`, `templates/<id>.txt`). Every template the service issues from has best-estimate wording for legal, product and compliance to start from. Each one starts with the line `[Placeholder wording for legal review. Not for issue.]`, and `ActivateTemplate` refuses any template whose wording still carries it (`placeholder_wording`), so a placeholder can be submitted and tested but never reach a customer. `tests/placeholders.rs` renders each one from the data its letter is really given, so each placeholder also records that letter's data contract. `|money` takes whole minor units as a number, or as the text of one, because a requested letter's detail is text.

**Wording by hash** (`wordings.rs`, `GET /v1/wordings/{text_hash}`). The core keeps only the hash of each definition a cover is bound to (`DefinitionBinding { id, version, text_hash }`). The service has the whole definitions library (`products/definitions/*.star`, including AIA's 34 published wordings) built in, indexed by the SHA-256 of each text. Staff (Entra role `staff`, with `x-ic-actor` and `x-ic-purpose`) read the words a cover holds as `{id, version, title, effective_from, text_hash, text}`. Each read is logged. A hash the library does not hold is not found, so another version's words never stand in for it. The route is signed in like downloads, and is absent when `IC_ENTRA_TENANT` is not set. When a definitions file is added to `products/definitions`, it must be added to `wordings::SOURCES` too, and a test fails until it is.

## Issuing

A caller asks the driver to issue a document with a stable document id and the data to render from. The driver renders from the active template version, encrypts the text and the data snapshot together, writes the object, and then proposes `IssueDocument` with the hashes, the object name and the wrapped key. The same request again returns the same document. A request the module refuses leaves no object behind.

A correction is a new document that supersedes the old one. Both stay visible; nothing is edited.

The module state holds ids, hashes, dates and delivery results. Names, addresses and the text are only in the vault, so a snapshot or a state hash exposes nobody.

## Delivery and proof

Each recipient has a chain of channels from their preferences. For a document that must be proven delivered (every mandatory notice, remediation letters, complaint outcomes, and anything issued with `prove_delivery`), post is always added as the last resort.

- **Email, SMS**: sent through Azure Communication Services. The provider's delivery report, arriving through Event Grid, is the proof. A bounce or a refusal moves to the next channel at once. A send not confirmed within `electronic_confirm_hours` (72 by default) falls back.
- **In-app inbox**: nothing is sent; the document is proven when it is opened.
- **Post**: a print file is placed in the print outbox container. It is deemed delivered `post_deemed_days` (5 by default) later unless it is returned. Returned post reverses a deemed delivery.
- **Push**: sent by the app services, which hold device registrations. This driver records it as not available and moves on.

Contact details are fetched from the customer master (`crates/customers`, `docs/customers.md`) at the moment of sending, for the relationship the document is sent under, and only a masked form is recorded. Email carries the text only for ordinary, personal and financial documents; insurance-sensitive and clinical documents send a pointer to the secure inbox. SMS always sends a pointer. The customer master also says when a person has died: nothing is then sent to them on any channel, each attempt is recorded as having no contact point, and the document shows as undelivered for staff to send to whoever acts for the estate.

Every provider call carries an id derived from the document, party, channel and attempt, so a retry after a crash is the same operation to the provider.

## PDF

The text is the document: it is what is rendered, hashed and stored. `pdf.rs` sets that text on A4 pages whenever a PDF is wanted, and the PDF is never stored. The first page carries the name and address 45 mm down, where a DL envelope's window shows them, then the title and the text; every page has the document id and `n / total` at its foot. Large print (`AlternateFormat::LargePrint`) is 18 point. The fonts are embedded as subsets with a map back to characters, so the file is small (about 20 KB), searchable and readable by a screen reader, and the same letter always gives the same bytes.

- **Email** that may carry the text attaches `<document>.pdf`. An email that only points to the secure inbox attaches nothing.
- **Post** writes `print/<document>/<party>-<attempt>.pdf` to the print outbox before the JSON instruction, which names it in `pdf`.
- **Download**: `GET /v1/documents/{id}/pdf` on the service (below), built on `Driver::pdf(id, fonts)`.

A character the font cannot draw is an error, never a gap. The email or print instruction then goes with the text alone (`pdf` is null) and the reason is logged, so no notice is held up. The bundled faces are Liberation Sans (SIL Open Font License, `crates/documents/fonts/OFL.txt`), which covers Latin scripts including the macron vowels of te reo Māori. The insurer's own typeface is set with `IC_DOCUMENTS_FONT_REGULAR` and `IC_DOCUMENTS_FONT_BOLD`, the paths of two TrueType files (both or neither, for example from a mounted config map); the service reads and checks them at start and refuses to start if either is missing or unusable.

**Listed in the channels** (`listing.rs`, agreed with the portal thread). Every document on a policy is a row in the channel module's read model, written by the service as `publish_rows` under module `documents`: key `{policy}/documents/{document}` (one row per policy the document covers), kind `document`, subject the document's owner, `sensitive` for insurance-sensitive and clinical documents, body `{document, title, kind, issued_on, language, supersedes}`. A document on no policy is not listed. A destroyed document's rows are removed. The module keeps the changes still to carry (`Query::Listing`, `AckListing` from caller `relay`), so an outage delays a row and loses none. Who sees a row is the channels' audience policy; the download route still applies the owner rule.

**Downloading.** With `IC_ENTRA_TENANT` and `IC_DOCUMENTS_AUDIENCE` set (both or neither; `IC_ENTRA_AUTHORITY` defaults to `https://login.microsoftonline.com`), the service serves `GET /v1/documents/{id}/pdf` to callers with an Entra ID token for that audience (`auth.rs`). The token's app role decides the rule; a token with several roles names one in `x-ic-caller`.

| Role | Must send | May download |
| --- | --- | --- |
| `portal` | `x-ic-party`: the customer asking | A document the party owns or was a recipient of (an adviser or attorney it was copied to). Any other document answers 404, the same as one that does not exist. |
| `staff` | `x-ic-actor` and `x-ic-purpose` | Any document except a clinical one (403), which stays with the claims team's tools. |
| anything else | | Nothing (403). |

A missing or invalid token is 401; a destroyed document is 410; the module or the tenant's signing keys unreachable is 503. The answer is `application/pdf`, `Cache-Control: no-store`, named `<id>.pdf`. Each download is logged with the document, role, workload, party or actor and purpose, never the contents. Without the two settings the route is not served (404).

## Notice clocks

A notice with a notice period (`minimum_notice_days` on its template) has a purpose: `renewal`, `lapse`, `premium_change`, `suspension`. Its clock starts on the date delivery is proven to every addressee. `notice_gate(policy, purpose, date)` answers whether the thing the notice warns of may take effect: no notice, waiting (not delivered, undeliverable, defective, or period still running), or clear.

When a notice is proven delivered the module emits `NoticeDelivered` and queues a `RecordNoticeDelivery` instruction for the core. An undeliverable notice emits `NoticeUndeliverable` and stops the clock for good until delivery is retried with corrected contact details.

## Notice defects (design addendum, entry 3)

A remediation case raises a defect against the notices that went out wrong. A defective notice is treated as not given: its gate closes. Each affected notice is corrected by a new notice that supersedes it and must itself be proven delivered. When it is, the clock restarts from that date, `ReopenPeriodOpened` is emitted with reference `<defect>:<original notice>`, and an `OpenReopenWindow` instruction is queued for the core so a choice the customer made on the wrong information can be made again. A defect cannot be closed while any notice is uncorrected, and closing records the control that should have caught it.

## Retention, holds and erasure

Each kind has a retention rule: a number of days from issue, or from the end of the policy for contractual documents. The default is 2,557 days (seven years); **the periods per kind need legal review before go-live**. When a period ends the module drops the document's wrapped key, which makes the stored object unreadable, and the driver deletes the object.

Where the period runs from issue it is known when the document is written, and the object is written with a locked immutability policy to that date, so nobody can delete it early. A legal hold on a document, party or policy stops destruction until it is released. Erasing a person (`ShredParty`) drops keys at once for everything not held or still within a period the insurer must keep, and the rest as each period ends.

## Events

The module publishes `doc-<seq>` events to the `documents-events` Event Hub: templates activated and disabled, documents issued and superseded, delivery outcomes, `NoticeDelivered`, `NoticeUndeliverable`, `NoticeDefectRaised`, `ReopenPeriodOpened`, `NoticeClockRestarted`, holds and destruction. Sequence numbers have no gaps; consumers de-duplicate on the id.

## Reading the core

The module host feeds the module every partition's events. `triggers.rs` maps them to the documents they require: a schedule after issue and every change to cover (including layers ended or cut by a claim or by the owner, `LayerEnded` and `LayerReduced`, and cover written back, `LayerAdded`), at most one a policy a business date (trigger `schedule:<policy>:<date>:PolicySchedule`, its `event` the part before the kind; a change later the same day is in that schedule and asks for no other), a lapse or suspension notice when standing changes, an avoidance notice (`AvoidanceNotice`, a mandatory notice) when a policy is avoided for non-disclosure (`PolicyAvoided`, not the status change that goes with it), a receipt when a payment is applied, a premium change notice (`PremiumChangeNotice`, purpose `discount_change`, detail `entitlement`, `tier`, `life`, `from`, `reference`) when a discount is lost (`EntitlementChanged` with `held: false`), a premium notice (`Invoice`) for each bill on a policy of its own (`PremiumDue` with no members and a positive total; a scheme's bill is group schemes' invoice). A trigger is a statement that a document is due. The service answers it (`required_documents.rs`, every documents cycle): it reads the policy from the partition that holds it (`locate`, then `policy_by_number`), takes the active template of the trigger's kind for the policy's product on the trigger's business date (question `template_for_kind`; a schedule also takes the active wording), and issues document `required:<trigger id>` to the policyholder (addressee, relationship `policyholder`) with a copy to each party in `copy_to`, with `trigger_event` the trigger's event, which clears it. Lapse, suspension and receipt documents are financial; the rest personal. A trigger with a notice purpose goes as that notice. The template's data is: `policy`, `product`, `status`, `inception`, `frequency`, `currency`, `covers` (each `cover`, `type`, `status`, `start`, `sum_insured` on the date and the `layers` in force), `premium` (the last period bill), `outstanding` and `oldest_unpaid_due` from billing, `name`, `date`, and the trigger's own detail (`receipt`, `allocated`, `to_suspense` for a receipt; `oldest_unpaid_due` for a standing change; `ended` for a status change; for a premium notice `receivable`, `period_start`, `period_end`, `base` (the premium before discounts), `discounts` (each `name` and `amount`, in the product's order), `net_premium`, `charges` (tax, levies and fees: the total less the premium) and `total`, which is the bill's). Never a loading, an exclusion or anything else about health. A product with no active template of a kind leaves its documents waiting and is reported once a pass; a policyholder with no contact point or who has died leaves the document for a person; a format the template is not offered in falls back to the ordinary one. A product sends premium notices by having an active `invoice` template; a bill on a product with none is dismissed, not left. Events older than `live_event_days` are recorded but trigger nothing, so a replay cannot send a flood of old letters.

**Interested parties.** The core reports every interest noted on a policy, such as an assignment to a lender, and its release (`Event::Interest`). The module keeps who holds each interest and between which dates, from events of any age. A trigger for a lapse notice (raised when the policy enters grace, which is the arrears notice), a suspension notice or the letter that a policy has ended or the avoidance notice lists in `copy_to` the parties whose interest stood on the event's date. `IssueDocument` refuses a lapse or suspension notice, and any document answering a trigger with copies owed, with `interested_party_missing` unless every party whose interest stands on the day of issue is a recipient. The notice clock still runs from delivery to the addressees. An interest stops being owed copies on the date its release takes effect. Question `interested_parties` (`policy`, `on`) answers who is owed a copy.

## Azure

| Need | Service | Notes |
|---|---|---|
| Vault | Blob Storage, container with version-level immutability | Object names `documents/<id>/<random>`. |
| Print outbox | Blob Storage container | The print house collects from it. |
| Key wrapping | Key Vault or Managed HSM, RSA key, `wrapKey`/`unwrapKey` | The key never leaves the vault. Wrapped values name the key version, so rotation needs no re-wrap. |
| Email, SMS | Communication Services | Entra ID authentication; delivery reports through an Event Grid subscription. |
| Events out | Event Hubs, hubs `documents-events` and `complaints-events`, one partition each | |
| Identity | Workload identity | No keys or connection strings anywhere. |

## Running it

- The module is hosted by the deployed node: `ic_documents::module_spec()` is listed in `ic_node::modules()`.
- The driver runs as the `ic-documents` binary (`service.rs`): a cycle every `IC_DOCUMENTS_CYCLE_SECONDS` (15 by default) that lets time pass, sends, deletes, instructs the core and publishes, and a webhook at `POST /v1/delivery-reports` for the Event Grid subscription on Communication Services delivery reports. The webhook requires the header `x-webhook-key`, set as a delivery property on the subscription from a Key Vault secret of at least 32 characters. Any number of copies can run.
- Instructions to the core go to the partition that holds the policy as caller `documents`: `RecordNoticeDelivery` when a notice with a clock is proven delivered, `OpenReopenWindow` when a corrected notice is. The idempotency key is the instruction's own ID. An instruction the core refuses stays in the module's outbox and is reported by every cycle until it is resolved.
- The same process runs the complaints relay (`complaints_relay.rs`): it keeps time for the complaints module, publishes its events to the hub `complaints-events`, and opens a conduct case for each licence event the commission module reports. See `docs/complaints.md`.
- Settings are listed on `service::Config`. The webhook key is read from the file `IC_DOCUMENTS_WEBHOOK_KEY_FILE` names when that is set, which is how the deployment supplies it. All are required except the container names, key name, hub names, conduct officer, listen address and cycle interval.
- **Deploying.** `infra/documents.bicep` creates the vault storage account and its two containers, the wrapping key, Communication Services, the two Event Hubs, the service's identity and its role assignments, and the Event Grid subscription for delivery reports. `deploy/k8s/documents.yaml` runs two copies of the service. The template is deployed twice: first without `deliveryReportsUrl`, then, once the service is reachable at its public address, with it, because Event Grid creates a webhook subscription only after calling it. Three things need a person: verifying the email sending domain and linking it to the Communication Services resource, buying the SMS number or short code, and routing `https://<host>/v1/delivery-reports` (that path only) through the ingress to the `ic-documents` service. The template and manifest have not been deployed from here; check the Communication Services role ID as the template's parameter describes before the first deployment.

**Idempotency keys** are remembered for 90 days (`KEEP_KEYS_DAYS`) and then forgotten as `Tick` passes, at most 200 a tick. Core events, `Tick` and `MarkPublished` are frequent and leave none.

**Letters for complaints and conduct cases.** Beside the relay, the service issues the letters the complaints register lists as owed and records each on its complaint or case (`complaint_letters.rs`; `docs/complaints.md`, "Letters").

**Letters other modules ask for** (`requested_letters.rs`). A module that cannot issue a document records `RequestLetter` with the facts: `{request, kind, recipient: {party: {party}} | {adviser: {adviser}}, policy, subject, life, reasons, detail, business_date}`. It sends codes and facts, never wording. The kinds each caller may ask for, and their templates, are `model::REQUESTED_KINDS`:

| Caller | Kind | Template | Class | Proven |
| --- | --- | --- | --- | --- |
| underwriting | offer_made | uw-offer-made | clinical | no |
| underwriting | declined | uw-declined | clinical | yes |
| underwriting | proposal_lapsed, proposal_withdrawn, policy_issued, interview_to_confirm | uw-<kind with dashes> | personal | no |
| underwriting | adviser_unavailable | uw-adviser-unavailable | personal | yes |
| reinsurance | reinsurance_statement | ri-reinsurance-statement | personal | yes |
| group | absence_started, absence_limit_reached | grp-absence-started, grp-absence-limit-reached | personal | yes |
| claims | overpayment_repayment | cl-overpayment-repayment | personal | yes |

Claims lives inside the partitions and cannot send a request, so the service pulls (`repayment_letters.rs`, agreed with the claims and payments threads): each pass asks every partition the claims question `repayment_letters`, asks payments for each overpayment's reference, records the letter as a request from claims (the request id is claims' own, `cl-op:<overpayment>:<n>`; one recorded earlier for a different amount stands), and sends the partition `RecordRepaymentLetter` as caller documents so it leaves the list. An overpayment payments says nothing is owed on is skipped; claims drops it itself. The overpayment letter must carry `detail.repayment_reference` (from module payments, `{"q":"repayment_reference","overpayment":…}`, "OP" and six digits) and `detail.amount`; without either it is refused with `missing_detail`. A credit quoting that reference is matched to the overpayment, so the template must print it.

Group sends the leave-of-absence letters to the member (the party on the scheme policy's life), with `policy` the scheme policy, `life` the life, and `detail` `since` and `cover_ends` (persona GRP-038).

A clinical letter goes to the person alone (`clinical_to_adviser` otherwise). Other refusals: `unknown_letter_kind`, `no_request`, `request_conflict` (same id, other content; an identical resend is accepted). The service issues each waiting request as document `letter:<request>` to the recipient as addressee, with `name`, `reference` (the subject), `policy`, `life`, `reasons`, `date` and the `detail` entries as data, and the issue marks the request issued. A request whose template has no wording, or whose person has no contact point or has died, waits and is reported every pass; `CancelLetterRequest` (the asking module or servicing, with a reason) drops one. `Query::LetterRequest` says where one stands; settled requests are forgotten after 90 days. An adviser is addressed by their party id in the customer master. Reinsurance listings (personal data) never go by email: Mitchell decided they reach reinsurers by SFTP and by an API the reinsurers call, both run by the reinsurance service. The statement letter carries totals only.

**Dunning notices from collections** (`collections_notices.rs`, agreed with the payments thread). The service asks module `collections` for `notices_due`, issues each as document `collections:<notice id>` from the notice's template (`collections/<step>`) to the customer as addressee on the step's channel, with every policy it covers, class financial, proof as the notice says, and `trigger_event` the document id. For a suspension or final notice every interested party standing on any of the policies on the notice date gets a copy. It then sends `record_notice_sent` (caller `delivery`, key `notice-sent:<id>`). A notice whose template has no wording waits. Collections templates must not carry a notice period: collections keeps the dunning clock. Each pass also asks for `notices_sent` and reports `record_notice_delivery` (key `notice-delivery:<id>`): delivered once every addressee's delivery is proven, undeliverable as soon as one addressee's has failed for good (collections then opens a task for a mandatory notice). A lender's copy does not decide it.

**Issuing twice.** `Driver::issue` keys the command on the document id and a digest of the request. The same request again changes nothing; a request that was refused and then put right is judged afresh.

## Not built, or waiting on others

- **Audio and braille.** The requested format is recorded on the document and passed to the print house, which produces audio and braille.
- **Tagged PDF.** Built 2026-11-13 (`pdf.rs`): every PDF is marked and has a structure tree (Document; a P for each address line and paragraph, the title as the one H1, in reading order; a paragraph crossing a page is one element), the page foot is a pagination artifact, pages tab in structure order, and XMP metadata gives the title, language and `pdfuaid:part` 1. Not done: a run through a PDF/UA checker such as veraPDF or PAC before go-live, which this environment does not have.
- **Scripts outside the typeface.** Text in a script the font lacks (Chinese, for example) goes as text without a PDF, and the download route answers 500 for it. A second face for such scripts is not set.
- **The documents app registration.** The portal and staff tools call the route inside the cluster; it is not routed through the ingress. `deploy/k8s/documents.yaml` sets the two Entra settings; `deploy/entra/apps.sh workforce` makes the app registration (audience, app roles `portal` and `staff`) and `apps.sh grant` gives those roles to the callers' identities. Running the script is a person's step, with their Entra rights.
- **Loading history.** A module added to a running cluster starts at the oldest event each partition still holds.
