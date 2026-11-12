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
- **Download**: `Driver::pdf(id, fonts)` gives the stored text as a PDF, for the portal and app.

A character the font cannot draw is an error, never a gap. The email or print instruction then goes with the text alone (`pdf` is null) and the reason is logged, so no notice is held up. The bundled faces are Liberation Sans (SIL Open Font License, `crates/documents/fonts/OFL.txt`), which covers Latin scripts including the macron vowels of te reo Māori.

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

The module host feeds the module every partition's events. `triggers.rs` maps them to the documents they require: a schedule after issue and every change to cover, a lapse or suspension notice when standing changes, a receipt when a payment is applied. A trigger is a statement that a document is due; a service with access to the policy and customer data answers it by asking the driver to issue. Events older than `live_event_days` are recorded but trigger nothing, so a replay cannot send a flood of old letters.

**Interested parties.** The core reports every interest noted on a policy, such as an assignment to a lender, and its release (`Event::Interest`). The module keeps who holds each interest and between which dates, from events of any age. A trigger for a lapse notice (raised when the policy enters grace, which is the arrears notice), a suspension notice or the letter that a policy has ended lists in `copy_to` the parties whose interest stood on the event's date. `IssueDocument` refuses a lapse or suspension notice, and any document answering a trigger with copies owed, with `interested_party_missing` unless every party whose interest stands on the day of issue is a recipient. The notice clock still runs from delivery to the addressees. An interest stops being owed copies on the date its release takes effect. Question `interested_parties` (`policy`, `on`) answers who is owed a copy.

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

**Issuing twice.** `Driver::issue` keys the command on the document id and a digest of the request. The same request again changes nothing; a request that was refused and then put right is judged afresh.

## Not built, or waiting on others

- **Audio, braille and tagged PDF.** The requested format is recorded on the document and passed to the print house, which produces audio and braille. The PDF records its language and title and can be read back as text, but it is not a tagged (PDF/UA) file.
- **The insurer's own typeface.** PDFs are set in the bundled Liberation Sans. `Fonts::new` and `AzureChannels::with_fonts` take other TrueType faces, but the service has no setting for them yet. Text in a script the font lacks (Chinese, for example) goes as text without a PDF.
- **Loading history.** A module added to a running cluster starts at the oldest event each partition still holds.
