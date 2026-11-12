# Complaints, disputes and conduct cases

`crates/complaints` (`ic-complaints`) is the complaints register, the timers every complaint runs under, referral to the external scheme and the courts, conduct cases with their affected populations, and the figures reported from all of it. It is a deterministic state machine replicated in its own group on the ledger nodes as module `complaints`. This page says how it works, who calls it, and what is not built.

## What it holds

IDs, dates, categories and what was done. The complainant's words, every letter and every bundle are documents in the vault (`docs/documents.md`); the module holds their IDs. No clinical detail and no free text about a person is in the state, so a snapshot or a state hash exposes nobody.

## Commands and callers

Every command arrives in an envelope: an idempotency key, the calling service, the person responsible and the proposer's time. A repeated key returns the original result. Time never runs backwards. Each command lists the services that may send it:

| Caller | What it sends |
|---|---|
| `config` | `ActivateTimers` |
| `intake` (app, portals, claims and servicing workbenches) | `RecordComplaint` |
| `complaints` (the complaints team's workbench) | everything on a complaint |
| `legal` | forum commands, conduct case commands, and the only caller for a court or the privacy regulator |
| `conduct` | conduct cases |
| `distribution` (commission and adviser module) | opening a licence case and attaching its population |
| `documents` | the acknowledgement, deadlock notice and licence letters it issues, and proof that an outcome letter was delivered |
| `scheduler` | `Tick` |
| `relay` | `MarkPublished` |
| `migration` | `RecordComplaint` with the original dates |

## Timers

`Timers` is versioned configuration from the conduct regime and the scheme's terms of reference: days to acknowledge, to update, to decide and to deadlock, the warning lead time, the authority limits, the regulator notification period, retention, and the cause codes. A version is never changed, and a complaint runs for its life under the version in force when it was received.

`Tick` raises each timer once at each level: a warning before it is due, the complaints lead when it is missed, the conduct officer when it is still not done. When the scheme's period ends without an outcome the complaint is deadlocked by the passing of time, and a deadlock notice is owed before it can close. A tick examines at most 500 complaints and carries on from where it stopped.

A complainant asked for something has a set time to answer, longer if they are marked vulnerable. A late answer prompts the handler once. The insurer's own timers never move.

## A complaint's life

1. **Recorded** by one command, wherever it is heard, by anyone, about anything: policies, cases, decisions with who made them and under which rule, an adviser engagement, staff. A service matter can be put right on the spot within a small limit, and is recorded all the same.
2. **Assigned** to someone who was not involved, whose authority is no lower than the decision under review, and who is trained if the complainant is marked vulnerable. Access to the file comes with the assignment and ends with it. Further access is granted per case, for named classes and a stated purpose; clinical access only to the assigned reviewer of a claims or underwriting decision.
3. **Decided** by its owner: upheld, partly upheld, not upheld, withdrawn or resolved by agreement, with reason codes and a written outcome that must be proven delivered.
4. **Remediated** through the ordinary command of the module that owns the change: a reopened decision, a refund, a goodwill payment within the authoriser's limit, a corrected record. The complaint holds the reference, never the adjustment.
5. **Referred**, if the complainant takes it further: the scheme, a court or the privacy regulator, with the forum's deadlines, bundles checked by one person and released by another, and a binding decision implemented against a date.
6. **Closed** only with an outcome or a forum's decision, proof of delivery, the deadlock notice if one was owed, all remediation done, and a cause code. An overturned decision is linked to who made it and the rule and wording version, for the quality loop in that function.
7. **Removed** when its retention ends (`retain_closed_days`). The count of removed records is kept.

A systemic finding names the remediation case opened for everyone else affected, and tells the conduct officer.

## Conduct cases

A conduct case is a restricted case type owned by the conduct officer: a licence event, a breach, an incident, an adviser pattern or a systemic finding.

**Licence events (design addendum, entry 4).** When a licence or authorisation is revoked, suspended or lapses, the commission module opens its own case and reports it in its `conduct_cases` answer. A module cannot call another while it applies a command, so the complaints relay in the documents service carries it across: on every pass it asks module `distribution` for its conduct cases, finds the policyholder of each affected policy from the partition that holds it, and proposes to this module, as caller `distribution`, `OpenConductCase` with `ConductKind::LicenceEvent` and the case id `licence:<event id>`, the customers, advisers and policies in `AttachPopulation` chunks of at most 500, and `CompletePopulation` with the counts the case then holds. Every one of these is safe to repeat, a pass cut short is finished by the next, and a case whose population is complete is left alone. A policy with no holder on record stops that case, and the pass reports it every time until it is resolved, because a population completed without that customer would leave them untold.

From there the case drives:

- **Communication.** `PopulationComplete` is the signal to write to each customer. Each is recorded as told, by a named document, or excluded for a stated reason.
- **Sampled review of advice files.** `DrawSample` orders the policies by SHA-256 of `<case>|<policy>` and takes the first `size`. Anyone can repeat the draw. Each file is reviewed once, never by one of the advisers, and a file that needs putting right names its remediation case.
- **The regulator.** A notification with its own timer, and each request for information with its due date, both escalating like any other timer.

A licence case closes only when the population is complete, every customer is told or excluded, the sample is drawn and reviewed, the regulator is notified and every request answered.

## Reading it

`Query` on the module answers: a complaint (only to someone the case has given access), the complainant's own record without staff names, an adviser's view (existence and outcome only), the work queue (vulnerable first, then nearest deadline, showing adjustments but not the reason), running timers, a party's complaints, the register and the report for a period, conduct cases (only to the conduct function and legal), timers, pending events and the state hash.

The report gives complaints by process, cause, product and channel; timers met and missed as they stand at the end of the period; outcomes; deadlocks; referrals and forum outcomes; money paid by kind; systemic findings; repeat and vulnerable complainants; overturns by decision maker and rule version; complaints by adviser engagement; months that ended without a root cause review; open conduct cases; records removed.

## Events

Events are numbered `cmp-<seq>` without gaps and wait in the module until the relay marks them published. Other services act on them: `AcknowledgementDue` and `DeadlockReached` ask the documents service for a letter, `Decided` carries the decisions overturned, `SystemicFindingRaised` and `TimerEscalated` reach the conduct officer and the lead, `ComplaintClosed` carries the adviser engagement for distribution oversight.

## Running it

The module is hosted by the deployed node: `ic_complaints::module_spec()` is listed in `ic_node::modules()`. Commands are proposed with `ClusterClient::propose_module_until("complaints", ...)` and questions asked with `query_module`.

The documents service (`ic-documents`, see `docs/documents.md`) runs the complaints relay beside its own cycle, every `IC_DOCUMENTS_CYCLE_SECONDS`:

- forwards licence cases from the commission module, owned by `IC_CONDUCT_OFFICER` (default `conduct-officer`);
- sends `Tick` as caller `scheduler`, so timers escalate, deadlock arrives and retention ends;
- publishes the module's events to the Event Hub `IC_COMPLAINTS_EVENT_HUB` (default `complaints-events`, one partition) and then sends `MarkPublished`. An event is acknowledged only after the hub has it; consumers de-duplicate on the id.

A step that fails is logged and tried again on the next pass, and does not stop the other steps.

### Letters

Question `letters_due` lists what is owed and nobody has issued: an acknowledgement for each open complaint without one (not migrated complaints), a deadlock notice for each deadlock without one, and a letter to each customer of a licence case whose population is complete and who is neither told nor excluded. It names parties and dates, nothing a complaint is about.

The documents service (`complaint_letters.rs`) works that list every pass. For each letter it asks the customer master for the person's name, language and format, issues the document from the template for its kind with proof of delivery required, to the person and anyone recorded as acting for them, and records it as caller `documents` with `RecordAcknowledgement`, `RecordDeadlockNotice` or `RecordCustomerNotified`. The document id is made from the complaint or case (`complaint-ack:<complaint>`, `complaint-deadlock:<complaint>`, `licence-notice:<case>:<party>`), so a letter is issued once however a pass is interrupted, and a letter a handler recorded first counts as done.

Templates receive `name`, `reference` (the complaint or case), `date` (received, deadlocked or opened), `due` (acknowledgements), and for a licence case `status` and `effective`. A letter is left for a person, and the pass says so in the log, when its template has no active version, the person has no contact point, or the customer master says they have died. Conduct cases other than licence events are always written to by the people handling them.

Idempotency keys are remembered for 90 days (`KEEP_KEYS_DAYS`) and then forgotten as `Tick` passes, at most 500 a tick. `Tick` and `MarkPublished` leave none.

## Not built, or waiting on others

- **The workbench** is built by the portal service: page `/complaints` and the routes under `/staff/v1/complaints` (`docs/channels.md`). It reads this module as JSON, and `crates/channels/tests/http.rs` runs it against `ComplaintsState`, so renaming a command, question or field here fails that test.
- **Wording.** The three letter templates (`complaint-acknowledgement`, `complaint-deadlock-notice`, `licence-event-notice`) are not written. Until legal, product and compliance approve and activate each, its letters are left for the people handling the cases.
- **The timer values** in tests are short for testing. The real periods come from the conduct regime and the scheme's terms and need compliance sign-off before go-live.
