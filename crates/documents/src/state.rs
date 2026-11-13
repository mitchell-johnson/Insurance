//! The documents state machine: command in, events out.
//!
//! Deterministic. It never reads a clock: time is what commands carry. It
//! holds no personal data: document text and the data it was rendered from
//! are in the vault, and this state holds their hashes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use ic_core::interest::InterestEvent;
use ic_core::{PartyId, PolicyId, PolicyStatus, Reject, Standing};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};

use crate::command::{ApprovalRole, Command, Envelope, HoldScope, Issue, LegacyImport, RenderCase, Settings};
use crate::event::{Event, EventRecord};
use crate::kinds::{DocumentClass, DocumentKind};
use crate::model::*;
use crate::template::{sha256_hex, Part, PartRef, Template, TemplateRef};

/// The most items one command may carry or one tick may process. A command
/// does a bounded amount of work, like every command in the core.
pub const MAX_CHUNK: usize = 200;

/// How long an idempotency key is remembered. A repeat of a command older
/// than this is judged on its merits, like a new one.
pub const KEEP_KEYS_DAYS: i64 = 90;

/// The default retention, in days, where no schedule has been activated:
/// seven years. The periods need legal review; see `docs/documents.md`.
pub const DEFAULT_RETENTION_DAYS: u32 = 2557;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateStatus {
    Draft,
    Active,
    Disabled,
}

/// Where a shared part (a clause) is used: the register a change to it is
/// judged against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartUsage {
    pub part: String,
    /// The newest registered version, if any is.
    pub latest: Option<u32>,
    /// Each template version that is not disabled and uses the part.
    pub templates: Vec<PartUse>,
    /// Every product version whose documents use the part.
    pub products: BTreeSet<crate::template::ProductBinding>,
    /// The product versions whose current template still uses an older
    /// version than the latest: the ones a change has yet to reach.
    pub behind: BTreeSet<crate::template::ProductBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartUse {
    pub template: TemplateRef,
    pub status: TemplateStatus,
    pub part_version: u32,
    pub products: Vec<crate::template::ProductBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderingRun {
    pub at: DateTime<Utc>,
    pub cases: u32,
    /// What failed, by persona, in words. Synthetic data only.
    pub failures: Vec<String>,
    /// The product versions and languages the passing cases covered.
    pub covered: BTreeSet<(String, u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRecord {
    pub template: Template,
    pub content_hash: String,
    /// The fields the template needs from the data.
    pub required_fields: BTreeSet<String>,
    pub status: TemplateStatus,
    pub approvals: BTreeMap<ApprovalRole, String>,
    pub last_run: Option<RenderingRun>,
    /// Every rendering run that had a failure, for reporting.
    pub failed_runs: Vec<DateTime<Utc>>,
    pub effective_from: Option<NaiveDate>,
    pub disabled: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredResult {
    index: u64,
    error: Option<Reject>,
}

/// What applying one command produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<EventRecord>,
    /// The key had been seen before: this is the original result.
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DocumentsState {
    pub settings: Settings,
    settings_versions: BTreeMap<u32, Settings>,
    pub retention_version: u32,
    retention: BTreeMap<DocumentKind, RetentionRule>,
    retention_versions: BTreeMap<u32, String>,
    #[serde(with = "pairs")]
    pub parts: BTreeMap<PartRef, Part>,
    #[serde(with = "pairs")]
    pub templates: BTreeMap<TemplateRef, TemplateRecord>,
    pub documents: BTreeMap<String, Document>,
    /// The owner keys, wrapped by the Key Vault key.
    pub party_keys: BTreeMap<PartyId, String>,
    by_policy: BTreeMap<PolicyId, Vec<String>>,
    by_party: BTreeMap<PartyId, Vec<String>>,
    /// Documents with a delivery still in progress.
    open_deliveries: BTreeSet<String>,
    pub defects: BTreeMap<String, Defect>,
    /// The last core event seen from each core partition.
    pub checkpoints: BTreeMap<u16, u64>,
    pub triggers: BTreeMap<String, Trigger>,
    pub core_outbox: BTreeMap<String, CoreInstruction>,
    /// Events not yet on the stream, oldest first.
    unpublished: VecDeque<EventRecord>,
    next_seq: u64,
    /// The latest time any command has carried.
    pub now: Option<DateTime<Utc>>,
    policy_ended: BTreeMap<PolicyId, NaiveDate>,
    /// Interests in each policy, by the core's interest id, as its events
    /// reported them.
    #[serde(default)]
    interests: BTreeMap<PolicyId, BTreeMap<String, StandingInterest>>,
    /// Destruction dates and the documents due on them.
    expiry: BTreeMap<NaiveDate, BTreeSet<String>>,
    #[serde(with = "pairs")]
    holds: BTreeMap<HoldScope, String>,
    shredding: BTreeSet<PartyId>,
    /// Vault objects to delete, by document.
    pub pending_deletes: BTreeMap<String, String>,
    results: BTreeMap<String, StoredResult>,
    /// The remembered keys, oldest first, with the day each was stored, so
    /// that the old ones can be forgotten.
    #[serde(default)]
    key_log: VecDeque<(NaiveDate, String)>,
    /// Letters other modules asked for, by their request id.
    #[serde(default)]
    pub letter_requests: BTreeMap<String, LetterRequestRecord>,
    /// The requests in the order they came, to forget settled ones.
    #[serde(default)]
    letter_log: VecDeque<(NaiveDate, String)>,
}

/// The local date in New Zealand, which is the date notices and retention
/// periods are counted in.
pub fn nz_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&chrono_tz::Pacific::Auckland).date_naive()
}

fn reject<T>(code: &str, message: impl Into<String>) -> Result<T, Reject> {
    Err(Reject::new(code, message))
}

fn days(n: u32) -> Duration {
    Duration::days(n as i64)
}

struct Ctx {
    at: DateTime<Utc>,
    today: NaiveDate,
    actor: String,
    caller: String,
    events: Vec<Event>,
}

impl StateMachine for DocumentsState {
    type Command = Envelope;
    type Output = Applied;

    fn apply(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(stored) = self.results.get(&env.key) {
            return Applied {
                index: stored.index,
                result: stored.error.clone().map_or(Ok(()), Err),
                events: vec![],
                duplicate: true,
            };
        }
        // Time never runs backwards, whatever a proposer's clock says.
        let at = self.now.map_or(env.at, |now| now.max(env.at));
        let mut ctx = Ctx {
            at,
            today: nz_date(at),
            actor: env.actor.clone(),
            caller: env.caller.clone(),
            events: vec![],
        };
        // Core events are de-duplicated by the checkpoint, and letting time
        // pass or acknowledging the stream twice changes nothing the second
        // time, so the keys of these frequent commands are not kept.
        let remember = !matches!(env.command, Command::ObserveCoreEvents { .. } | Command::Tick | Command::MarkPublished { .. });
        let is_tick = matches!(env.command, Command::Tick);
        let today = ctx.today;
        let result = if !env.command.allowed_callers().contains(&env.caller.as_str()) {
            reject("not_allowed", format!("{} may not send {}", env.caller, env.command.name()))
        } else if env.actor.is_empty() {
            reject("no_actor", "a command names who is responsible for it")
        } else {
            self.dispatch(env.command, &mut ctx)
        };
        // A rejected command leaves no trace but its stored result: every
        // handler validates before it writes, and emits only after.
        let events: Vec<EventRecord> = if result.is_ok() {
            self.now = Some(at);
            ctx.events
                .into_iter()
                .map(|event| {
                    self.next_seq += 1;
                    EventRecord {
                        id: format!("doc-{}", self.next_seq),
                        seq: self.next_seq,
                        at,
                        log_index: index,
                        event,
                    }
                })
                .collect()
        } else {
            vec![]
        };
        self.unpublished.extend(events.iter().cloned());
        if remember {
            self.key_log.push_back((today, env.key.clone()));
            self.results.insert(
                env.key,
                StoredResult {
                    index,
                    error: result.clone().err(),
                },
            );
        } else if is_tick {
            self.forget_old_keys(today);
            self.forget_old_letters(today);
        }
        Applied {
            index,
            result,
            events,
            duplicate: false,
        }
    }
}

impl DocumentsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A key guards against a command sent twice, which happens within
    /// minutes. Keys older than [`KEEP_KEYS_DAYS`] are forgotten as time
    /// passes, a bounded number at a time, so the state does not grow for ever.
    fn forget_old_keys(&mut self, today: NaiveDate) {
        for _ in 0..MAX_CHUNK {
            match self.key_log.front() {
                Some((stored, _)) if *stored + Duration::days(KEEP_KEYS_DAYS) < today => {}
                _ => break,
            }
            if let Some((_, key)) = self.key_log.pop_front() {
                self.results.remove(&key);
            }
        }
    }

    fn request_letter(&mut self, request: LetterRequest, ctx: &mut Ctx) -> Result<(), Reject> {
        if request.request.trim().is_empty() || request.subject.trim().is_empty() {
            return reject("no_request", "a letter request names its request and its subject");
        }
        let Some(kind) = requested_kind(&ctx.caller, &request.kind) else {
            return reject(
                "unknown_letter_kind",
                format!("{} may not ask for a letter of kind {}", ctx.caller, request.kind),
            );
        };
        if kind.clinical && matches!(request.recipient, LetterRecipient::Adviser { .. }) {
            return reject("clinical_to_adviser", "a letter about a person's health goes to that person, not their adviser");
        }
        if let Some(existing) = self.letter_requests.get(&request.request) {
            if existing.request == request && existing.caller == ctx.caller {
                return Ok(());
            }
            return reject("request_conflict", format!("request {} was made before with other content", request.request));
        }
        let id = request.request.clone();
        ctx.events.push(Event::LetterRequested {
            request: id.clone(),
            caller: ctx.caller.clone(),
            kind: request.kind.clone(),
        });
        self.letter_log.push_back((ctx.today, id.clone()));
        self.letter_requests.insert(
            id,
            LetterRequestRecord {
                request,
                caller: ctx.caller.clone(),
                received: ctx.today,
                status: LetterRequestStatus::Pending,
            },
        );
        Ok(())
    }

    fn cancel_letter_request(&mut self, request: &str, reason: String, ctx: &mut Ctx) -> Result<(), Reject> {
        if reason.trim().is_empty() {
            return reject("no_reason", "cancelling a letter needs a reason");
        }
        let Some(record) = self.letter_requests.get_mut(request) else {
            return reject("unknown_request", format!("no letter was asked for under {request}"));
        };
        if ctx.caller != "servicing" && ctx.caller != record.caller {
            return reject("not_yours", "only the module that asked, or servicing, may cancel a letter");
        }
        if record.status != LetterRequestStatus::Pending {
            return reject("not_pending", "the letter has already been issued or cancelled");
        }
        record.status = LetterRequestStatus::Cancelled { reason };
        ctx.events.push(Event::LetterRequestCancelled { request: request.to_string() });
        Ok(())
    }

    /// A document issued under a requested letter's id settles it.
    fn letter_issued(&mut self, document: &str) {
        let Some(request) = document.strip_prefix("letter:") else { return };
        if let Some(record) = self.letter_requests.get_mut(request) {
            if record.status == LetterRequestStatus::Pending {
                record.status = LetterRequestStatus::Issued {
                    document: document.to_string(),
                };
            }
        }
    }

    /// Where a part is used, and which products a new version has not yet
    /// reached. A template counts once its newest version that is not
    /// disabled is considered: an older version it replaced does not hold a
    /// product behind.
    pub fn part_usage(&self, part: &str) -> PartUsage {
        let latest = self.parts.keys().filter(|r| r.id == part).map(|r| r.version).max();
        let mut usage = PartUsage {
            part: part.to_string(),
            latest,
            templates: vec![],
            products: BTreeSet::new(),
            behind: BTreeSet::new(),
        };
        let mut newest: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
        for (r, record) in &self.templates {
            if record.status == TemplateStatus::Disabled {
                continue;
            }
            let Some(used) = record.template.parts.iter().find(|p| p.id == part) else {
                continue;
            };
            usage.templates.push(PartUse {
                template: r.clone(),
                status: record.status,
                part_version: used.version,
                products: record.template.products.clone(),
            });
            usage.products.extend(record.template.products.iter().cloned());
            let entry = newest.entry(r.id.as_str()).or_insert((r.version, used.version));
            if r.version >= entry.0 {
                *entry = (r.version, used.version);
            }
        }
        for (id, (version, part_version)) in newest {
            if latest.is_some_and(|l| part_version < l) {
                let r = TemplateRef { id: id.to_string(), version };
                usage.behind.extend(self.templates[&r].template.products.iter().cloned());
            }
        }
        usage
    }

    /// The requested letters still to issue, oldest first.
    pub fn letters_requested(&self, limit: usize) -> Vec<&LetterRequestRecord> {
        self.letter_log
            .iter()
            .filter_map(|(_, id)| self.letter_requests.get(id))
            .filter(|r| r.status == LetterRequestStatus::Pending)
            .take(limit)
            .collect()
    }

    /// Settled requests are forgotten after the keys are. A request still
    /// pending is kept, and moved to the back so that it holds up nothing.
    fn forget_old_letters(&mut self, today: NaiveDate) {
        for _ in 0..MAX_CHUNK {
            let Some((stored, id)) = self.letter_log.front().cloned() else { break };
            if stored + Duration::days(KEEP_KEYS_DAYS) >= today {
                break;
            }
            self.letter_log.pop_front();
            match self.letter_requests.get(&id).map(|r| &r.status) {
                Some(LetterRequestStatus::Pending) => self.letter_log.push_back((today, id)),
                _ => {
                    self.letter_requests.remove(&id);
                }
            }
        }
    }

    /// How many idempotency keys are remembered.
    pub fn remembered_keys(&self) -> usize {
        self.results.len()
    }

    fn dispatch(&mut self, command: Command, ctx: &mut Ctx) -> Result<(), Reject> {
        match command {
            Command::ActivateSettings(s) => self.activate_settings(s, ctx),
            Command::ActivateRetention { version, rules } => self.activate_retention(version, rules, ctx),
            Command::RegisterPart(part) => self.register_part(part, ctx),
            Command::SubmitTemplate(t) => self.submit_template(*t, ctx),
            Command::ApproveTemplate { template, role } => self.approve_template(template, role, ctx),
            Command::RunRenderingTests { template, cases } => self.run_rendering_tests(template, cases, ctx),
            Command::ActivateTemplate { template, effective_from } => self.activate_template(template, effective_from, ctx),
            Command::DisableTemplate { template, reason, case } => self.disable_template(template, reason, case, ctx),
            Command::RegisterPartyKey { party, wrapped_key } => self.register_party_key(party, wrapped_key),
            Command::IssueDocument(issue) => {
                let id = issue.document.clone();
                self.issue(*issue, ctx)?;
                self.letter_issued(&id);
                Ok(())
            }
            Command::RequestLetter(request) => self.request_letter(request, ctx),
            Command::CancelLetterRequest { request, reason } => self.cancel_letter_request(&request, reason, ctx),
            Command::ImportLegacy(import) => self.import_legacy(*import, ctx),
            Command::RecordSend {
                document,
                party,
                channel,
                masked_destination,
                provider_reference,
            } => self.record_send(&document, &party, channel, masked_destination, provider_reference, ctx),
            Command::RecordOutcome {
                document,
                party,
                channel,
                outcome,
            } => self.record_outcome(&document, &party, channel, outcome, ctx),
            Command::RetryDelivery { document, party, channels } => self.retry_delivery(&document, &party, channels, ctx),
            Command::Tick => {
                self.tick(ctx);
                Ok(())
            }
            Command::RaiseNoticeDefect {
                defect,
                case,
                description,
                cause,
                from,
                to,
                grace_days,
            } => self.raise_defect(defect, case, description, cause, from, to, grace_days, ctx),
            Command::AttachDefectNotices { defect, notices } => self.attach_defect_notices(&defect, notices, ctx),
            Command::ExcludeFromDefect { defect, notice, reason } => self.exclude_from_defect(&defect, &notice, reason, ctx),
            Command::CloseNoticeDefect { defect, control } => self.close_defect(&defect, control, ctx),
            Command::ObserveCoreEvents { partition, records } => self.observe(partition, records, ctx),
            Command::DismissTrigger { trigger, reason } => {
                if reason.is_empty() {
                    return reject("no_reason", "dismissing a required document needs a reason");
                }
                if self.triggers.remove(&trigger).is_none() {
                    return reject("unknown_trigger", format!("no document is waiting on {trigger}"));
                }
                ctx.events.push(Event::TriggerDismissed { trigger });
                Ok(())
            }
            Command::AckCoreInstructions { ids } => {
                for id in ids {
                    self.core_outbox.remove(&id);
                }
                Ok(())
            }
            Command::MarkPublished { through_seq } => {
                while self.unpublished.front().is_some_and(|e| e.seq <= through_seq) {
                    self.unpublished.pop_front();
                }
                Ok(())
            }
            Command::PlaceLegalHold { scope, case } => {
                if case.is_empty() {
                    return reject("no_case", "a legal hold names the case that needs it");
                }
                self.holds.insert(scope, case.clone());
                ctx.events.push(Event::LegalHoldPlaced { case });
                Ok(())
            }
            Command::ReleaseLegalHold { scope } => {
                if self.holds.remove(&scope).is_none() {
                    return reject("no_hold", "there is no such legal hold");
                }
                ctx.events.push(Event::LegalHoldReleased);
                Ok(())
            }
            Command::ShredParty { party } => self.shred_party(party, ctx),
            Command::AckDeleted { documents } => {
                for id in documents {
                    self.pending_deletes.remove(&id);
                }
                Ok(())
            }
        }
    }

    // --- configuration ---

    fn activate_settings(&mut self, s: Settings, ctx: &mut Ctx) -> Result<(), Reject> {
        if s.version == 0 {
            return reject("invalid", "settings versions start at one");
        }
        match self.settings_versions.get(&s.version) {
            Some(existing) if *existing == s => return Ok(()),
            Some(_) => {
                return reject(
                    "config_conflict",
                    format!("settings version {} is already active with other content", s.version),
                )
            }
            None => {}
        }
        if s.version < self.settings.version {
            return reject("config_conflict", "a newer settings version is already active");
        }
        self.settings_versions.insert(s.version, s.clone());
        ctx.events.push(Event::SettingsActivated { version: s.version });
        self.settings = s;
        Ok(())
    }

    fn activate_retention(&mut self, version: u32, rules: BTreeMap<DocumentKind, RetentionRule>, ctx: &mut Ctx) -> Result<(), Reject> {
        let hash = sha256_hex(&serde_json::to_vec(&rules).unwrap_or_default());
        match self.retention_versions.get(&version) {
            Some(existing) if *existing == hash => return Ok(()),
            Some(_) => return reject("config_conflict", format!("retention schedule {version} is already active with other content")),
            None => {}
        }
        if version <= self.retention_version {
            return reject("config_conflict", "a newer retention schedule is already active");
        }
        if let Some(kind) = DocumentKind::ALL.iter().find(|k| !rules.contains_key(k)) {
            return reject("invalid", format!("the schedule has no rule for {kind:?}"));
        }
        self.retention_versions.insert(version, hash);
        self.retention_version = version;
        self.retention = rules;
        ctx.events.push(Event::RetentionActivated { version });
        Ok(())
    }

    /// How long documents of a kind are kept, and from when.
    pub fn retention_rule(&self, kind: DocumentKind) -> RetentionRule {
        self.retention.get(&kind).copied().unwrap_or(RetentionRule {
            basis: if kind.class() == DocumentClass::Contractual {
                RetentionBasis::PolicyEnd
            } else {
                RetentionBasis::Issue
            },
            days: DEFAULT_RETENTION_DAYS,
        })
    }

    fn register_part(&mut self, part: Part, ctx: &mut Ctx) -> Result<(), Reject> {
        if part.id.is_empty() || part.version == 0 || part.bodies.is_empty() {
            return reject("invalid", "a part needs an id, a version and at least one language");
        }
        let r = PartRef {
            id: part.id.clone(),
            version: part.version,
        };
        match self.parts.get(&r) {
            Some(existing) if *existing == part => return Ok(()),
            Some(_) => return reject("config_conflict", format!("part {} version {} exists with other content", r.id, r.version)),
            None => {}
        }
        ctx.events.push(Event::PartRegistered {
            id: r.id.clone(),
            version: r.version,
            content_hash: part.content_hash(),
        });
        self.parts.insert(r, part);
        Ok(())
    }

    fn submit_template(&mut self, template: Template, ctx: &mut Ctx) -> Result<(), Reject> {
        let r = template.reference();
        if let Some(existing) = self.templates.get(&r) {
            return if existing.template == template {
                Ok(())
            } else {
                reject("config_conflict", format!("template {} version {} exists with other content", r.id, r.version))
            };
        }
        if template.author != ctx.actor {
            return reject("invalid", "the template's author is whoever submits it");
        }
        let required_fields = template
            .validate(&self.parts, self.settings.max_reading_grade_x10)
            .map_err(|e| Reject::new("invalid_template", e.to_string()))?;
        let content_hash = template.content_hash(&self.parts);
        ctx.events.push(Event::TemplateSubmitted {
            template: r.clone(),
            content_hash: content_hash.clone(),
        });
        self.templates.insert(
            r,
            TemplateRecord {
                template,
                content_hash,
                required_fields,
                status: TemplateStatus::Draft,
                approvals: BTreeMap::new(),
                last_run: None,
                failed_runs: vec![],
                effective_from: None,
                disabled: None,
            },
        );
        Ok(())
    }

    fn template_mut(&mut self, r: &TemplateRef) -> Result<&mut TemplateRecord, Reject> {
        self.templates
            .get_mut(r)
            .ok_or_else(|| Reject::new("unknown_template", format!("there is no template {} version {}", r.id, r.version)))
    }

    fn approve_template(&mut self, r: TemplateRef, role: ApprovalRole, ctx: &mut Ctx) -> Result<(), Reject> {
        let actor = ctx.actor.clone();
        let record = self.template_mut(&r)?;
        if record.status != TemplateStatus::Draft {
            return reject("not_draft", "only a draft is approved");
        }
        if record.template.author == actor {
            return reject("maker_checker", "an author does not approve their own template");
        }
        if record.approvals.iter().any(|(held, by)| *held != role && *by == actor) {
            return reject("maker_checker", "one person does not give two of a template's approvals");
        }
        record.approvals.insert(role, actor);
        ctx.events.push(Event::TemplateApproved { template: r, role });
        Ok(())
    }

    fn run_rendering_tests(&mut self, r: TemplateRef, cases: Vec<RenderCase>, ctx: &mut Ctx) -> Result<(), Reject> {
        if cases.is_empty() || cases.len() > MAX_CHUNK {
            return reject("invalid", format!("a rendering run has between one and {MAX_CHUNK} cases"));
        }
        let parts = self.parts.clone();
        let at = ctx.at;
        let record = self.template_mut(&r)?;
        if record.status != TemplateStatus::Draft {
            return reject("not_draft", "rendering tests gate activation, so they run on a draft");
        }
        let mut failures = Vec::new();
        let mut covered = BTreeSet::new();
        for case in &cases {
            let label = format!("{} v{} {} ({})", case.product_code, case.product_version, case.persona, case.language);
            let supplied: BTreeSet<&String> = case.product_fields.iter().collect();
            if let Some(missing) = record.required_fields.iter().find(|f| !supplied.contains(f)) {
                failures.push(format!("{label}: the product does not supply `{missing}`"));
                continue;
            }
            match record.template.render(&parts, &case.language, &case.data) {
                Err(e) => failures.push(format!("{label}: {e}")),
                Ok(text) if sha256_hex(text.as_bytes()) != case.expected_sha256 => {
                    failures.push(format!("{label}: the rendered document is not the expected one"))
                }
                Ok(_) => {
                    covered.insert((case.product_code.clone(), case.product_version, case.language.clone()));
                }
            }
        }
        if !failures.is_empty() {
            record.failed_runs.push(at);
        }
        ctx.events.push(Event::RenderingTested {
            template: r,
            cases: cases.len() as u32,
            failures: failures.len() as u32,
        });
        record.last_run = Some(RenderingRun {
            at,
            cases: cases.len() as u32,
            failures,
            covered,
        });
        Ok(())
    }

    fn activate_template(&mut self, r: TemplateRef, effective_from: NaiveDate, ctx: &mut Ctx) -> Result<(), Reject> {
        let record = self.template_mut(&r)?;
        match record.status {
            TemplateStatus::Active if record.effective_from == Some(effective_from) => return Ok(()),
            TemplateStatus::Draft => {}
            _ => return reject("config_conflict", "an active or disabled template version is immutable; submit a new version"),
        }
        let mut needed = vec![ApprovalRole::Legal, ApprovalRole::Product];
        if record.template.kind.needs_compliance_approval() {
            needed.push(ApprovalRole::Compliance);
        }
        if let Some(missing) = needed.iter().find(|role| !record.approvals.contains_key(role)) {
            return reject("not_approved", format!("the template lacks {missing:?} approval"));
        }
        let Some(run) = &record.last_run else {
            return reject("not_tested", "the template has no rendering test run");
        };
        if !run.failures.is_empty() {
            return reject("tests_failed", format!("the last rendering run had {} failures", run.failures.len()));
        }
        for language in record.template.languages() {
            if record.template.products.is_empty() {
                if !run.covered.iter().any(|(_, _, l)| l == language) {
                    return reject("not_tested", format!("no rendering test covers the `{language}` version"));
                }
            } else if let Some(p) = record
                .template
                .products
                .iter()
                .find(|p| !run.covered.contains(&(p.code.clone(), p.version, language.clone())))
            {
                return reject(
                    "not_tested",
                    format!("no rendering test covers {} version {} in `{language}`", p.code, p.version),
                );
            }
        }
        record.status = TemplateStatus::Active;
        record.effective_from = Some(effective_from);
        ctx.events.push(Event::TemplateActivated {
            template: r,
            content_hash: record.content_hash.clone(),
            effective_from,
        });
        Ok(())
    }

    fn disable_template(&mut self, r: TemplateRef, reason: String, case: String, ctx: &mut Ctx) -> Result<(), Reject> {
        if reason.is_empty() || case.is_empty() {
            return reject("invalid", "disabling a template needs a reason and a case");
        }
        let record = self.template_mut(&r)?;
        match record.status {
            TemplateStatus::Disabled => return Ok(()),
            TemplateStatus::Draft => return reject("not_active", "a draft is not in use, so there is nothing to disable"),
            TemplateStatus::Active => {}
        }
        record.status = TemplateStatus::Disabled;
        record.disabled = Some((reason, case.clone()));
        ctx.events.push(Event::TemplateDisabled { template: r, case });
        Ok(())
    }

    /// The version of a template to render with on a date: the active one
    /// with the latest effective date not after it.
    pub fn template_for(&self, id: &str, on: NaiveDate) -> Option<&TemplateRecord> {
        self.templates
            .values()
            .filter(|t| t.template.id == id && t.status == TemplateStatus::Active && t.effective_from.is_some_and(|d| d <= on))
            .max_by_key(|t| (t.effective_from, t.template.version))
    }

    // --- documents ---

    fn register_party_key(&mut self, party: PartyId, wrapped_key: String) -> Result<(), Reject> {
        if wrapped_key.is_empty() {
            return reject("invalid", "the wrapped key is empty");
        }
        if self.shredding.contains(&party) {
            return reject("shredded", "the party is being shredded and takes no new key");
        }
        match self.party_keys.get(&party) {
            Some(existing) if *existing == wrapped_key => Ok(()),
            // The first key stands: a second writer must use it.
            Some(_) => reject("key_exists", "the party already has a key"),
            None => {
                self.party_keys.insert(party, wrapped_key);
                Ok(())
            }
        }
    }

    fn check_storable(&self, document: &str, owner: &PartyId, content_sha256: &str, blob: &str, wrapped_key: &str) -> Result<(), Reject> {
        if document.is_empty() || self.documents.contains_key(document) {
            return reject("document_exists", format!("document id `{document}` is empty or already used"));
        }
        if content_sha256.len() != 64 || blob.is_empty() || wrapped_key.is_empty() {
            return reject("invalid", "a document needs its SHA-256, its vault object and its wrapped key");
        }
        if !self.party_keys.contains_key(owner) {
            return reject("no_party_key", "the owner has no key, so the document cannot have been encrypted under it");
        }
        Ok(())
    }

    fn issue(&mut self, issue: Issue, ctx: &mut Ctx) -> Result<(), Reject> {
        self.check_storable(&issue.document, &issue.owner, &issue.content_sha256, &issue.blob, &issue.wrapped_key)?;
        let Some(record) = self.templates.get(&issue.template) else {
            return reject(
                "unknown_template",
                format!("there is no template {} version {}", issue.template.id, issue.template.version),
            );
        };
        if record.status == TemplateStatus::Disabled {
            return reject("template_disabled", "the template version is disabled and issues nothing");
        }
        if self.template_for(&issue.template.id, ctx.today).map(|t| t.template.version) != Some(issue.template.version) {
            return reject(
                "template_not_current",
                "the document was not rendered with the template version in effect today",
            );
        }
        if record.template.kind != issue.kind {
            return reject("wrong_kind", format!("the template renders {:?}, not {:?}", record.template.kind, issue.kind));
        }
        if !record.template.bodies.contains_key(&issue.language) {
            return reject("no_language", format!("the template has no `{}` version", issue.language));
        }
        if issue.format.is_some_and(|f| !record.template.alternate_formats.contains(&f)) {
            return reject("no_format", "the template is not available in that format");
        }
        if issue.data_sha256.len() != 64 {
            return reject("invalid", "a rendered document records the hash of the data it came from");
        }
        if issue.recipients.is_empty() || issue.recipients.len() > MAX_CHUNK {
            return reject("invalid", "a document has at least one recipient");
        }
        if !issue.recipients.iter().any(|r| r.role == RecipientRole::Addressee) {
            return reject("invalid", "a document is addressed to someone");
        }
        let mut seen = BTreeSet::new();
        if issue.recipients.iter().any(|r| !seen.insert(&r.party)) {
            return reject("invalid", "a party receives a document once");
        }
        let proof = issue.kind.requires_proof() || issue.prove_delivery;
        if issue.other_policies.len() > MAX_CHUNK || (issue.policy.is_none() && !issue.other_policies.is_empty()) {
            return reject("invalid", "a document covering several policies names its first as its policy");
        }
        if issue.recipients.iter().any(|r| r.channels.is_empty() && !proof) {
            return reject("invalid", "every recipient needs a channel");
        }
        if let Some(policy) = &issue.policy {
            let from_trigger = issue
                .trigger_event
                .as_ref()
                .and_then(|event| self.triggers.get(&format!("{event}:{:?}", issue.kind)))
                .is_some_and(|t| !t.copy_to.is_empty());
            if from_trigger || matches!(issue.kind, DocumentKind::LapseNotice | DocumentKind::SuspensionNotice) {
                // A lender the policy is assigned to must hear that it is
                // in arrears, suspended or ended as the owner does.
                let owed = self.interested_parties(policy, ctx.today);
                if let Some(missing) = owed.iter().find(|p| !issue.recipients.iter().any(|r| r.party == **p)) {
                    return reject(
                        "interested_party_missing",
                        format!("{} has an interest in the policy and must receive this document", missing.0),
                    );
                }
            }
        }
        if issue.kind.class() == DocumentClass::MandatoryNotice && issue.trigger_event.is_none() {
            // A notice renders after the event that requires it, so it can
            // never say something the ledger does not.
            return reject("no_trigger", "a mandatory notice names the event that requires it");
        }
        if issue.kind == DocumentKind::PolicySchedule {
            if issue.policy.is_none() {
                return reject("invalid", "a schedule belongs to a policy");
            }
            if issue.wordings.is_empty() {
                return reject("no_wording", "a schedule names the wording version it is issued under");
            }
            if let Some(w) = issue.wordings.iter().find(|w| {
                !self
                    .templates
                    .get(w)
                    .is_some_and(|t| t.template.kind == DocumentKind::PolicyWording && t.status != TemplateStatus::Draft)
            }) {
                return reject("no_wording", format!("{} version {} is not an activated policy wording", w.id, w.version));
            }
        }
        let minimum_days = record.template.minimum_notice_days;
        let notice = match (&issue.notice, minimum_days) {
            (None, Some(_)) => {
                return reject(
                    "no_purpose",
                    "the template carries a notice period, so the document must say what it is notice of",
                )
            }
            (None, None) => None,
            (Some(spec), _) => {
                if !proof {
                    return reject("invalid", "only a document that must be proven delivered can be a notice with a clock");
                }
                if spec.purpose.is_empty() || issue.policy.is_none() {
                    return reject("invalid", "a notice with a clock has a purpose and a policy");
                }
                Some(Notice {
                    purpose: spec.purpose.clone(),
                    minimum_days: minimum_days.unwrap_or(0),
                    delivered_on: None,
                    earliest_effect: None,
                    undeliverable: false,
                    defect: None,
                    corrects: spec.corrects.clone(),
                })
            }
        };
        // A correction replaces the notice it corrects.
        let correction = notice.as_ref().and_then(|n| n.corrects.clone());
        let supersedes = match (&correction, &issue.supersedes) {
            (Some(c), Some(s)) if c.original != *s => return reject("invalid", "a corrected notice supersedes the notice it corrects"),
            (Some(c), _) => Some(c.original.clone()),
            (None, s) => s.clone(),
        };
        if let Some(c) = &correction {
            let Some(defect) = self.defects.get(&c.defect) else {
                return reject("unknown_defect", format!("there is no notice defect {}", c.defect));
            };
            if defect.closed.is_some() {
                return reject("defect_closed", "the notice defect is closed");
            }
            match defect.notices.get(&c.original) {
                Some(DefectItem::Outstanding) => {}
                Some(DefectItem::Corrected { by }) if self.documents.get(by).is_some_and(|d| d.notice.as_ref().is_some_and(|n| n.undeliverable)) => {}
                Some(_) => return reject("already_corrected", "the notice already has a correction, or is excluded"),
                None => return reject("not_in_defect", "the notice is not under that defect"),
            }
        }
        if let Some(earlier_id) = &supersedes {
            let Some(earlier) = self.documents.get(earlier_id) else {
                return reject("unknown_document", format!("there is no document {earlier_id} to supersede"));
            };
            // A correction of a corrected notice that could not be
            // delivered replaces that one, which already replaced the original.
            let chain_end = correction.is_some() && earlier.superseded_by.is_some();
            if earlier.superseded_by.is_some() && !chain_end {
                return reject("already_superseded", "the earlier document already has a later version");
            }
            if earlier.kind != issue.kind || earlier.policy != issue.policy || earlier.owner != issue.owner {
                return reject("invalid", "a new version is of the same kind, policy and owner as the one it replaces");
            }
            if issue.reason.as_ref().is_none_or(String::is_empty) {
                return reject("no_reason", "a correction is a new version with a reason");
            }
        }
        if let (Some(c), Some(n)) = (&correction, &notice) {
            let original = self.documents.get(&c.original).and_then(|d| d.notice.as_ref());
            if original.is_some_and(|o| o.purpose != n.purpose) {
                return reject("invalid", "a corrected notice has the purpose of the notice it corrects");
            }
        }
        // The version actually replaced: the end of the chain of versions.
        let replaced = supersedes.as_ref().map(|first| {
            let mut at = first.clone();
            while let Some(next) = self.documents.get(&at).and_then(|d| d.superseded_by.clone()) {
                at = next;
            }
            at
        });

        // Validated. Write.
        let template_hash = record.content_hash.clone();
        let deliveries: Vec<Delivery> = issue
            .recipients
            .iter()
            .map(|recipient| {
                let mut chain = recipient.channels.clone();
                chain.dedup();
                if proof && !chain.contains(&Channel::Post) {
                    chain.push(Channel::Post);
                }
                Delivery {
                    recipient: recipient.clone(),
                    attempts: vec![Attempt {
                        channel: chain[0],
                        requested_at: ctx.at,
                        sent_at: None,
                        masked_destination: None,
                        provider_reference: None,
                        outcome: None,
                        outcome_at: None,
                    }],
                    chain,
                    status: DeliveryStatus::InProgress,
                }
            })
            .collect();
        ctx.events.push(Event::DocumentIssued {
            document: issue.document.clone(),
            kind: issue.kind,
            policy: issue.policy.clone(),
            owner: issue.owner.clone(),
            template: Some(issue.template.clone()),
            content_sha256: issue.content_sha256.clone(),
            supersedes: replaced.clone(),
        });
        for d in &deliveries {
            ctx.events.push(Event::DeliveryRequested {
                document: issue.document.clone(),
                party: d.recipient.party.clone(),
                channel: d.chain[0],
            });
        }
        if let Some(at) = &replaced {
            if let Some(earlier) = self.documents.get_mut(at) {
                earlier.superseded_by = Some(issue.document.clone());
            }
            ctx.events.push(Event::DocumentSuperseded {
                document: at.clone(),
                by: issue.document.clone(),
            });
        }
        if let Some(c) = &correction {
            if let Some(defect) = self.defects.get_mut(&c.defect) {
                defect.notices.insert(c.original.clone(), DefectItem::Corrected { by: issue.document.clone() });
            }
        }
        if let Some(event) = &issue.trigger_event {
            self.triggers.remove(&format!("{event}:{:?}", issue.kind));
        }
        let document = Document {
            id: issue.document.clone(),
            kind: issue.kind,
            template: Some(issue.template),
            template_hash: Some(template_hash),
            policy: issue.policy,
            other_policies: issue.other_policies,
            data_class: issue.data_class,
            lives: issue.lives,
            proof,
            owner: issue.owner,
            language: issue.language,
            format: issue.format,
            content_sha256: issue.content_sha256,
            data_sha256: Some(issue.data_sha256),
            blob: issue.blob,
            wrapped_key: Some(issue.wrapped_key),
            bytes: issue.bytes,
            issued_at: ctx.at,
            issued_on: ctx.today,
            trigger_event: issue.trigger_event,
            case: issue.case,
            reason: issue.reason,
            supersedes: replaced,
            superseded_by: None,
            wordings: issue.wordings,
            components: issue.components,
            notice,
            deliveries,
            destroy_after: None,
            destroyed_on: None,
            legacy: false,
        };
        self.store(document);
        self.open_deliveries.insert(issue.document);
        Ok(())
    }

    fn import_legacy(&mut self, import: LegacyImport, ctx: &mut Ctx) -> Result<(), Reject> {
        self.check_storable(&import.document, &import.owner, &import.content_sha256, &import.blob, &import.wrapped_key)?;
        if import.issued_on > ctx.today {
            return reject("invalid", "a legacy document was issued in the past");
        }
        ctx.events.push(Event::DocumentIssued {
            document: import.document.clone(),
            kind: import.kind,
            policy: import.policy.clone(),
            owner: import.owner.clone(),
            template: None,
            content_sha256: import.content_sha256.clone(),
            supersedes: None,
        });
        self.store(Document {
            id: import.document,
            kind: import.kind,
            template: None,
            template_hash: None,
            policy: import.policy,
            other_policies: vec![],
            data_class: import.data_class,
            lives: vec![],
            proof: false,
            owner: import.owner,
            language: import.language,
            format: None,
            content_sha256: import.content_sha256,
            data_sha256: None,
            blob: import.blob,
            wrapped_key: Some(import.wrapped_key),
            bytes: import.bytes,
            issued_at: ctx.at,
            // The retention clock runs from the original date.
            issued_on: import.issued_on,
            trigger_event: None,
            case: None,
            reason: None,
            supersedes: None,
            superseded_by: None,
            wordings: vec![],
            components: vec![],
            notice: None,
            deliveries: vec![],
            destroy_after: None,
            destroyed_on: None,
            legacy: true,
        });
        Ok(())
    }

    fn store(&mut self, mut document: Document) {
        for policy in document.policies() {
            self.by_policy.entry(policy.clone()).or_default().push(document.id.clone());
        }
        let mut parties: BTreeSet<&PartyId> = document.deliveries.iter().map(|d| &d.recipient.party).collect();
        parties.insert(&document.owner);
        for party in parties {
            self.by_party.entry(party.clone()).or_default().push(document.id.clone());
        }
        document.destroy_after = self.destroy_after(&document);
        if let Some(date) = document.destroy_after {
            self.expiry.entry(date).or_default().insert(document.id.clone());
        }
        self.documents.insert(document.id.clone(), document);
    }

    fn destroy_after(&self, document: &Document) -> Option<NaiveDate> {
        let rule = self.retention_rule(document.kind);
        let from = match (rule.basis, &document.policy) {
            (RetentionBasis::PolicyEnd, Some(policy)) => *self.policy_ended.get(policy)?,
            _ => document.issued_on,
        };
        Some(from + days(rule.days))
    }

    // --- delivery ---

    fn delivery_mut(&mut self, document: &str, party: &PartyId) -> Result<(&mut Document, usize), Reject> {
        let doc = self
            .documents
            .get_mut(document)
            .ok_or_else(|| Reject::new("unknown_document", format!("there is no document {document}")))?;
        let i = doc
            .deliveries
            .iter()
            .position(|d| d.recipient.party == *party)
            .ok_or_else(|| Reject::new("unknown_recipient", "the document was not sent to that party"))?;
        Ok((doc, i))
    }

    fn record_send(&mut self, document: &str, party: &PartyId, channel: Channel, masked: String, reference: String, ctx: &mut Ctx) -> Result<(), Reject> {
        if masked.is_empty() {
            return reject("invalid", "the delivery record keeps a masked destination");
        }
        let (doc, i) = self.delivery_mut(document, party)?;
        let Some(attempt) = doc.deliveries[i].attempts.last_mut().filter(|a| a.channel == channel && a.outcome.is_none()) else {
            return reject("no_attempt", "no send on that channel is waiting");
        };
        if attempt.sent_at.is_none() {
            attempt.sent_at = Some(ctx.at);
            attempt.masked_destination = Some(masked);
            attempt.provider_reference = Some(reference);
        }
        Ok(())
    }

    fn record_outcome(&mut self, document: &str, party: &PartyId, channel: Channel, outcome: AttemptOutcome, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let at = ctx.at;
        let (doc, i) = self.delivery_mut(document, party)?;
        let delivery = &mut doc.deliveries[i];
        // Post that was deemed delivered and then comes back was not delivered.
        if outcome == AttemptOutcome::Returned {
            if channel != Channel::Post {
                return reject("invalid", "only post is returned");
            }
            let deemed = matches!(delivery.status, DeliveryStatus::Delivered { channel: Channel::Post, .. });
            let waiting = delivery.attempts.last().is_some_and(|a| a.channel == Channel::Post && a.outcome.is_none());
            if !deemed && !waiting {
                return reject("no_attempt", "no post to that party is outstanding");
            }
        } else if !delivery.attempts.last().is_some_and(|a| a.channel == channel && a.outcome.is_none()) {
            return reject("no_attempt", "no send on that channel is waiting for an outcome");
        }
        let attempt = delivery.attempts.last_mut().expect("checked above");
        attempt.outcome = Some(outcome.clone());
        attempt.outcome_at = Some(at);
        let id = doc.id.clone();
        let party = party.clone();
        match outcome {
            AttemptOutcome::Delivered => {
                delivery.status = DeliveryStatus::Delivered { on: today, channel };
                ctx.events.push(Event::DeliveryConfirmed {
                    document: id.clone(),
                    party,
                    channel,
                    on: today,
                });
            }
            failed => {
                if matches!(failed, AttemptOutcome::Bounced { .. } | AttemptOutcome::Returned) {
                    ctx.events.push(Event::ContactPointBounced { party: party.clone(), channel });
                }
                ctx.events.push(Event::DeliveryFailed {
                    document: id.clone(),
                    party,
                    channel,
                });
                delivery.status = DeliveryStatus::InProgress;
                Self::fall_back(doc, i, at, ctx);
            }
        }
        self.after_delivery_change(&id, ctx);
        Ok(())
    }

    /// Move a delivery to its next channel, or give up.
    fn fall_back(doc: &mut Document, i: usize, at: DateTime<Utc>, ctx: &mut Ctx) {
        let proof = doc.requires_proof();
        let delivery = &mut doc.deliveries[i];
        let tried = delivery.attempts.len();
        // A document that need not be proven is sent once, on the first channel.
        let next = if proof { delivery.chain.get(tried).copied() } else { None };
        match next {
            Some(channel) => {
                delivery.attempts.push(Attempt {
                    channel,
                    requested_at: at,
                    sent_at: None,
                    masked_destination: None,
                    provider_reference: None,
                    outcome: None,
                    outcome_at: None,
                });
                ctx.events.push(Event::DeliveryRequested {
                    document: doc.id.clone(),
                    party: delivery.recipient.party.clone(),
                    channel,
                });
            }
            None => {
                delivery.status = DeliveryStatus::Undeliverable;
                ctx.events.push(Event::DocumentUndeliverable {
                    document: doc.id.clone(),
                    party: delivery.recipient.party.clone(),
                });
            }
        }
    }

    fn retry_delivery(&mut self, document: &str, party: &PartyId, channels: Vec<Channel>, ctx: &mut Ctx) -> Result<(), Reject> {
        if channels.is_empty() {
            return reject("invalid", "a retry names the channels to try");
        }
        let at = ctx.at;
        let (doc, i) = self.delivery_mut(document, party)?;
        if doc.destroyed_on.is_some() {
            return reject("destroyed", "the document has been destroyed");
        }
        if doc.deliveries[i].status != DeliveryStatus::Undeliverable {
            return reject("not_undeliverable", "only an undeliverable document is retried");
        }
        let proof = doc.requires_proof();
        let delivery = &mut doc.deliveries[i];
        let mut chain = channels;
        chain.dedup();
        if proof && !chain.contains(&Channel::Post) {
            chain.push(Channel::Post);
        }
        // The attempts so far stay on the record; the new chain follows them.
        let first = chain[0];
        let mut full: Vec<Channel> = delivery.attempts.iter().map(|a| a.channel).collect();
        full.extend(chain);
        delivery.chain = full;
        delivery.status = DeliveryStatus::InProgress;
        delivery.attempts.push(Attempt {
            channel: first,
            requested_at: at,
            sent_at: None,
            masked_destination: None,
            provider_reference: None,
            outcome: None,
            outcome_at: None,
        });
        ctx.events.push(Event::DeliveryRequested {
            document: document.to_string(),
            party: party.clone(),
            channel: first,
        });
        if let Some(notice) = &mut doc.notice {
            notice.undeliverable = false;
        }
        self.open_deliveries.insert(document.to_string());
        Ok(())
    }

    /// After any delivery of a document changed: settle its notice clock.
    fn after_delivery_change(&mut self, id: &str, ctx: &mut Ctx) {
        let Some(doc) = self.documents.get_mut(id) else { return };
        if doc.deliveries.iter().all(|d| d.status != DeliveryStatus::InProgress) {
            self.open_deliveries.remove(id);
        } else {
            self.open_deliveries.insert(id.to_string());
        }
        let policy = doc.policy.clone();
        let addressees: Vec<&DeliveryStatus> = doc
            .deliveries
            .iter()
            .filter(|d| d.recipient.role == RecipientRole::Addressee)
            .map(|d| &d.status)
            .collect();
        let any_undeliverable = addressees.iter().any(|s| **s == DeliveryStatus::Undeliverable);
        let proven: Option<NaiveDate> = addressees
            .iter()
            .map(|s| match s {
                DeliveryStatus::Delivered { on, .. } => Some(*on),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .and_then(|dates| dates.into_iter().max());
        let Some(notice) = &mut doc.notice else { return };
        if notice.defect.is_some() {
            // A defective notice is treated as not given, whatever its delivery.
            return;
        }
        if any_undeliverable {
            if !notice.undeliverable {
                notice.undeliverable = true;
                ctx.events.push(Event::NoticeUndeliverable {
                    notice: id.to_string(),
                    policy: policy.clone(),
                    purpose: notice.purpose.clone(),
                });
            }
            // Post that was deemed delivered and came back: the clock stops.
            notice.delivered_on = None;
            notice.earliest_effect = None;
            let prefix = format!("notice-delivery:{id}:");
            self.core_outbox.retain(|key, _| !key.starts_with(&prefix));
            return;
        }
        let Some(delivered_on) = proven else { return };
        if notice.delivered_on.is_some() {
            return;
        }
        let earliest_effect = delivered_on + days(notice.minimum_days);
        notice.delivered_on = Some(delivered_on);
        notice.earliest_effect = Some(earliest_effect);
        let purpose = notice.purpose.clone();
        let corrects = notice.corrects.clone();
        ctx.events.push(match &corrects {
            None => Event::NoticeDelivered {
                notice: id.to_string(),
                policy: policy.clone(),
                purpose: purpose.clone(),
                delivered_on,
                earliest_effect,
            },
            Some(_) => Event::NoticeClockRestarted {
                notice: id.to_string(),
                policy: policy.clone(),
                purpose: purpose.clone(),
                delivered_on,
                earliest_effect,
            },
        });
        // The core is told once for each policy the notice covers.
        let policies: Vec<PolicyId> = self.documents.get(id).map(|d| d.policies().cloned().collect()).unwrap_or_default();
        for policy_id in &policies {
            self.core_outbox.insert(
                format!("notice-delivery:{id}:{policy_id}"),
                CoreInstruction::RecordNoticeDelivery {
                    policy: policy_id.clone(),
                    purpose: purpose.clone(),
                    notice: id.to_string(),
                    delivered_on,
                    earliest_effect,
                },
            );
        }
        if let Some(c) = corrects {
            let Some(defect) = self.defects.get_mut(&c.defect) else { return };
            let reference = format!("{}:{}", c.defect, c.original);
            let until = delivered_on + days(defect.grace_days);
            defect.notices.insert(
                c.original.clone(),
                DefectItem::Reopened {
                    by: id.to_string(),
                    reference: reference.clone(),
                    from: delivered_on,
                    until,
                },
            );
            ctx.events.push(Event::ReopenPeriodOpened {
                reopen_reference: reference.clone(),
                defect: c.defect,
                policy,
                purpose: purpose.clone(),
                corrected_notice: id.to_string(),
                from: delivered_on,
                until,
            });
            for policy_id in policies {
                self.core_outbox.insert(
                    format!("reopen:{reference}:{policy_id}"),
                    CoreInstruction::OpenReopenWindow {
                        policy: policy_id,
                        reopen_reference: reference.clone(),
                        purpose: purpose.clone(),
                        from: delivered_on,
                        until,
                    },
                );
            }
        }
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        let confirm = Duration::hours(self.settings.electronic_confirm_hours as i64);
        let deemed = self.settings.post_deemed_days;
        let at = ctx.at;
        let today = ctx.today;
        let mut changed = Vec::new();
        for id in self.open_deliveries.iter() {
            if changed.len() >= MAX_CHUNK {
                break;
            }
            let Some(doc) = self.documents.get_mut(id) else { continue };
            let proof = doc.requires_proof();
            let mut touched = false;
            for i in 0..doc.deliveries.len() {
                if doc.deliveries[i].status != DeliveryStatus::InProgress {
                    continue;
                }
                let Some(attempt) = doc.deliveries[i].attempts.last_mut() else { continue };
                let (Some(sent_at), None) = (attempt.sent_at, &attempt.outcome) else {
                    continue;
                };
                if attempt.channel == Channel::Post {
                    if today >= nz_date(sent_at) + days(deemed) {
                        attempt.outcome = Some(AttemptOutcome::Delivered);
                        attempt.outcome_at = Some(at);
                        doc.deliveries[i].status = DeliveryStatus::Delivered {
                            on: today,
                            channel: Channel::Post,
                        };
                        ctx.events.push(Event::DeliveryConfirmed {
                            document: id.clone(),
                            party: doc.deliveries[i].recipient.party.clone(),
                            channel: Channel::Post,
                            on: today,
                        });
                        touched = true;
                    }
                } else if at - sent_at >= confirm {
                    // Unconfirmed in the time allowed. A document that must
                    // be proven moves on; any other is simply done.
                    let channel = attempt.channel;
                    attempt.outcome = Some(AttemptOutcome::TimedOut);
                    attempt.outcome_at = Some(at);
                    if proof {
                        ctx.events.push(Event::DeliveryFailed {
                            document: id.clone(),
                            party: doc.deliveries[i].recipient.party.clone(),
                            channel,
                        });
                        Self::fall_back(doc, i, at, ctx);
                    } else {
                        doc.deliveries[i].status = DeliveryStatus::Delivered { on: nz_date(sent_at), channel };
                    }
                    touched = true;
                }
            }
            if touched {
                changed.push(id.clone());
            }
        }
        for id in changed {
            self.after_delivery_change(&id, ctx);
        }
        self.expire(ctx);
    }

    // --- notice defects ---

    fn raise_defect(
        &mut self,
        id: String,
        case: String,
        description: String,
        cause: DefectCause,
        from: NaiveDate,
        to: NaiveDate,
        grace_days: Option<u32>,
        ctx: &mut Ctx,
    ) -> Result<(), Reject> {
        if id.is_empty() || case.is_empty() || description.is_empty() {
            return reject("invalid", "a notice defect names itself, its remediation case and the defect");
        }
        if from > to {
            return reject("invalid", "the defect's period ends before it starts");
        }
        if self.defects.contains_key(&id) {
            return reject("defect_exists", format!("notice defect {id} already exists"));
        }
        self.defects.insert(
            id.clone(),
            Defect {
                id,
                case,
                description,
                cause,
                from,
                to,
                grace_days: grace_days.unwrap_or(self.settings.reopen_grace_days),
                raised_by: ctx.actor.clone(),
                notices: BTreeMap::new(),
                closed: None,
            },
        );
        Ok(())
    }

    fn open_defect(&mut self, id: &str) -> Result<&mut Defect, Reject> {
        let defect = self
            .defects
            .get_mut(id)
            .ok_or_else(|| Reject::new("unknown_defect", format!("there is no notice defect {id}")))?;
        if defect.closed.is_some() {
            return reject("defect_closed", "the notice defect is closed");
        }
        Ok(defect)
    }

    fn attach_defect_notices(&mut self, id: &str, notices: Vec<String>, ctx: &mut Ctx) -> Result<(), Reject> {
        if notices.is_empty() || notices.len() > MAX_CHUNK {
            return reject("invalid", format!("a chunk has between one and {MAX_CHUNK} notices"));
        }
        let (from, to, case) = {
            let d = self.open_defect(id)?;
            (d.from, d.to, d.case.clone())
        };
        for n in &notices {
            let Some(doc) = self.documents.get(n) else {
                return reject("unknown_document", format!("there is no document {n}"));
            };
            if !doc.requires_proof() {
                return reject("not_a_notice", format!("{n} is not a document that had to be proven"));
            }
            if doc.issued_on < from || doc.issued_on > to {
                return reject("outside_period", format!("{n} was not issued in the defect's period"));
            }
            if doc.notice.as_ref().is_some_and(|x| x.defect.as_deref().is_some_and(|d| d != id)) {
                return reject("already_defective", format!("{n} is already under another defect"));
            }
        }
        for n in notices {
            let doc = self.documents.get_mut(&n).expect("checked above");
            let purpose = match &mut doc.notice {
                Some(notice) => {
                    if notice.defect.is_some() {
                        continue;
                    }
                    notice.defect = Some(id.to_string());
                    // Treated as not given: its clock no longer stands.
                    notice.delivered_on = None;
                    notice.earliest_effect = None;
                    notice.purpose.clone()
                }
                // A proven letter with no clock can be defective too.
                None => String::new(),
            };
            let prefix = format!("notice-delivery:{n}:");
            self.core_outbox.retain(|key, _| !key.starts_with(&prefix));
            let defect = self.defects.get_mut(id).expect("checked above");
            if defect.notices.contains_key(&n) {
                continue;
            }
            defect.notices.insert(n.clone(), DefectItem::Outstanding);
            ctx.events.push(Event::NoticeDefectRaised {
                defect: id.to_string(),
                case: case.clone(),
                notice: n,
                policy: doc.policy.clone(),
                purpose,
            });
        }
        Ok(())
    }

    fn exclude_from_defect(&mut self, id: &str, notice: &str, reason: String, ctx: &mut Ctx) -> Result<(), Reject> {
        if reason.is_empty() {
            return reject("no_reason", "an exclusion states its reason");
        }
        let defect = self.open_defect(id)?;
        match defect.notices.get(notice) {
            None => return reject("not_in_defect", "the notice is not under that defect"),
            Some(DefectItem::Reopened { .. }) => return reject("already_corrected", "the notice is already corrected and its choices reopened"),
            Some(_) => {}
        }
        defect.notices.insert(notice.to_string(), DefectItem::Excluded { reason });
        ctx.events.push(Event::NoticeExcludedFromDefect {
            defect: id.to_string(),
            notice: notice.to_string(),
        });
        Ok(())
    }

    fn close_defect(&mut self, id: &str, control: String, ctx: &mut Ctx) -> Result<(), Reject> {
        if control.is_empty() {
            return reject("no_control", "closing a defect records the control that should have caught it");
        }
        let actor = ctx.actor.clone();
        let today = ctx.today;
        let defect = self.open_defect(id)?;
        if defect.raised_by == actor {
            return reject("maker_checker", "whoever raised a defect does not close it");
        }
        if defect.notices.is_empty() {
            return reject("empty", "the defect has no notices attached");
        }
        // Reconciliation: every affected notice corrected and proven, or
        // explicitly excluded with a reason.
        let open = defect
            .notices
            .values()
            .filter(|i| matches!(i, DefectItem::Outstanding | DefectItem::Corrected { .. }))
            .count();
        if open > 0 {
            return reject(
                "not_reconciled",
                format!("{open} notices have no corrected notice proven delivered and no exclusion"),
            );
        }
        let excluded = defect.notices.values().filter(|i| matches!(i, DefectItem::Excluded { .. })).count() as u32;
        defect.closed = Some(DefectClosure { by: actor, on: today, control });
        ctx.events.push(Event::NoticeDefectClosed {
            defect: id.to_string(),
            notices: defect.notices.len() as u32,
            excluded,
        });
        Ok(())
    }

    // --- the core's events ---

    fn observe(&mut self, partition: u16, records: Vec<ic_core::EventRecord>, ctx: &mut Ctx) -> Result<(), Reject> {
        if records.len() > MAX_CHUNK {
            return reject("invalid", format!("at most {MAX_CHUNK} events at a time"));
        }
        // A module added to a running cluster starts at the oldest event the
        // partition still holds: the first event seen sets the checkpoint.
        let checkpoint = match (self.checkpoints.get(&partition), records.first()) {
            (Some(seen), _) => *seen,
            (None, Some(first)) => first.seq.saturating_sub(1),
            (None, None) => return Ok(()),
        };
        let mut expected = checkpoint;
        for r in &records {
            if r.partition != partition {
                return reject("wrong_partition", "an event is from another partition");
            }
            // At least once: what was already seen is skipped. A gap is a defect.
            if r.seq <= checkpoint {
                continue;
            }
            if r.seq != expected + 1 {
                return reject("gap", format!("partition {partition}: expected event {}, got {}", expected + 1, r.seq));
            }
            expected = r.seq;
        }
        let cutoff = ctx.today - days(self.settings.live_event_days);
        for r in records.into_iter().filter(|r| r.seq > checkpoint) {
            self.note_policy_end(&r, ctx);
            // Who holds an interest is a standing fact, wanted however old
            // the event that reported it.
            self.note_interest(&r);
            if r.business_date < cutoff {
                // Old events are history being replayed, not news.
                continue;
            }
            for mut trigger in crate::triggers::triggers(&r) {
                if copied_to_interests(trigger.kind) {
                    trigger.copy_to = self.interested_parties(&trigger.policy, trigger.business_date);
                }
                ctx.events.push(Event::DocumentRequired {
                    trigger: trigger.id.clone(),
                    policy: trigger.policy.clone(),
                    kind: trigger.kind,
                });
                self.triggers.insert(trigger.id.clone(), trigger);
            }
        }
        self.checkpoints.insert(partition, expected);
        Ok(())
    }

    /// A lender or other party with an interest in a policy is owed a copy
    /// of the notices that warn it may end: the core's events say who they are.
    fn note_interest(&mut self, record: &ic_core::EventRecord) {
        let ic_core::Event::Interest(event) = &record.event else { return };
        match event {
            InterestEvent::Recorded {
                policy,
                interest,
                party,
                effective_from,
                ..
            } => {
                self.interests.entry(policy.clone()).or_default().insert(
                    interest.clone(),
                    StandingInterest {
                        party: party.clone(),
                        from: *effective_from,
                        until: None,
                    },
                );
            }
            InterestEvent::Released {
                policy, interest, effective, ..
            } => {
                if let Some(held) = self.interests.get_mut(policy).and_then(|of| of.get_mut(interest)) {
                    held.until = Some(*effective);
                }
            }
            InterestEvent::ConsentRecorded { .. } | InterestEvent::ConsentUsed { .. } => {}
        }
    }

    /// The parties whose interest in a policy stands on a date, each once.
    pub fn interested_parties(&self, policy: &PolicyId, on: NaiveDate) -> Vec<PartyId> {
        let standing: BTreeSet<&PartyId> = self
            .interests
            .get(policy)
            .into_iter()
            .flat_map(|of| of.values())
            .filter(|i| i.stands_on(on))
            .map(|i| &i.party)
            .collect();
        standing.into_iter().cloned().collect()
    }

    /// Contractual documents are kept for the life of the policy and a
    /// period after: the core's events say when a policy's life ended.
    fn note_policy_end(&mut self, record: &ic_core::EventRecord, ctx: &mut Ctx) {
        let (policy, ended) = match &record.event {
            ic_core::Event::PolicyStatusChanged { policy, to, .. } => (
                policy,
                matches!(
                    to,
                    PolicyStatus::Lapsed
                        | PolicyStatus::Surrendered
                        | PolicyStatus::Matured
                        | PolicyStatus::Expired
                        | PolicyStatus::Cancelled
                        | PolicyStatus::Terminated
                        | PolicyStatus::Avoided
                ),
            ),
            ic_core::Event::PolicyReinstated { policy } => (policy, false),
            _ => return,
        };
        if ended {
            self.policy_ended.insert(policy.clone(), record.business_date);
        } else {
            self.policy_ended.remove(policy);
        }
        let ids = self.by_policy.get(policy).cloned().unwrap_or_default();
        for id in ids {
            let Some(doc) = self.documents.get(&id) else { continue };
            if doc.destroyed_on.is_some() {
                continue;
            }
            let before = doc.destroy_after;
            let after = self.destroy_after(doc);
            if before == after {
                continue;
            }
            if let Some(date) = before {
                if let Some(set) = self.expiry.get_mut(&date) {
                    set.remove(&id);
                }
            }
            if let Some(date) = after {
                self.expiry.entry(date).or_default().insert(id.clone());
            }
            if let Some(doc) = self.documents.get_mut(&id) {
                doc.destroy_after = after;
            }
        }
        let _ = ctx;
    }

    // --- retention ---

    fn held(&self, doc: &Document) -> bool {
        self.holds.contains_key(&HoldScope::Document(doc.id.clone()))
            || self.holds.contains_key(&HoldScope::Party(doc.owner.clone()))
            || doc.policy.as_ref().is_some_and(|p| self.holds.contains_key(&HoldScope::Policy(p.clone())))
    }

    fn destroy(&mut self, id: &str, ctx: &mut Ctx) {
        let Some(doc) = self.documents.get_mut(id) else { return };
        // Without its key the vault object can never be read again, even
        // before the object itself is deleted.
        doc.wrapped_key = None;
        doc.destroyed_on = Some(ctx.today);
        if let Some(date) = doc.destroy_after {
            if let Some(set) = self.expiry.get_mut(&date) {
                set.remove(id);
            }
        }
        self.pending_deletes.insert(id.to_string(), doc.blob.clone());
        self.open_deliveries.remove(id);
        let owner = doc.owner.clone();
        ctx.events.push(Event::DocumentDestroyed { document: id.to_string() });
        self.finish_shred(&owner, ctx);
    }

    /// Destroy what has reached the end of its retention, unless held.
    fn expire(&mut self, ctx: &mut Ctx) {
        let due: Vec<String> = self
            .expiry
            .range(..ctx.today)
            .flat_map(|(_, ids)| ids.iter().cloned())
            .filter(|id| self.documents.get(id).is_some_and(|d| !self.held(d)))
            .take(MAX_CHUNK)
            .collect();
        for id in due {
            self.destroy(&id, ctx);
        }
        self.expiry.retain(|_, ids| !ids.is_empty());
    }

    fn shred_party(&mut self, party: PartyId, ctx: &mut Ctx) -> Result<(), Reject> {
        if !self.party_keys.contains_key(&party) && !self.shredding.contains(&party) {
            return reject("unknown_party", "the party has no key here");
        }
        if self.holds.contains_key(&HoldScope::Party(party.clone())) {
            return reject("legal_hold", "the party is under a legal hold");
        }
        if self.shredding.insert(party.clone()) {
            ctx.events.push(Event::PartyShredRequested { party: party.clone() });
        }
        // Informational documents and generated evidence go now. Contractual
        // documents and mandatory notices wait for their own retention to end.
        let now: Vec<String> = self
            .by_party
            .get(&party)
            .into_iter()
            .flatten()
            .filter(|id| {
                self.documents.get(*id).is_some_and(|d| {
                    d.owner == party
                        && d.destroyed_on.is_none()
                        && !self.held(d)
                        && matches!(d.kind.class(), DocumentClass::Informational | DocumentClass::GeneratedEvidence)
                        && !d.kind.requires_proof()
                })
            })
            .take(MAX_CHUNK)
            .cloned()
            .collect();
        for id in now {
            self.destroy(&id, ctx);
        }
        self.finish_shred(&party, ctx);
        Ok(())
    }

    /// Once a shredded party's last document is gone, their key goes too.
    fn finish_shred(&mut self, party: &PartyId, ctx: &mut Ctx) {
        if !self.shredding.contains(party) || !self.party_keys.contains_key(party) {
            return;
        }
        let remaining = self
            .by_party
            .get(party)
            .into_iter()
            .flatten()
            .any(|id| self.documents.get(id).is_some_and(|d| d.owner == *party && d.destroyed_on.is_none()));
        if !remaining {
            self.party_keys.remove(party);
            ctx.events.push(Event::PartyKeyDestroyed { party: party.clone() });
        }
    }

    // --- questions ---

    /// Events not yet on the stream, oldest first.
    pub fn pending_events(&self, limit: usize) -> Vec<EventRecord> {
        self.unpublished.iter().take(limit).cloned().collect()
    }

    /// Sends the delivery driver should make now.
    pub fn pending_sends(&self, limit: usize) -> Vec<PendingSend> {
        self.open_deliveries
            .iter()
            .filter_map(|id| self.documents.get(id))
            .flat_map(|doc| {
                doc.deliveries.iter().filter_map(move |d| {
                    let attempt = d.attempts.last()?;
                    (d.status == DeliveryStatus::InProgress && attempt.sent_at.is_none() && attempt.outcome.is_none()).then(|| PendingSend {
                        document: doc.id.clone(),
                        party: d.recipient.party.clone(),
                        channel: attempt.channel,
                        kind: doc.kind,
                        format: doc.format,
                    })
                })
            })
            .take(limit)
            .collect()
    }

    /// Everything a party may see, newest first: every version, with the
    /// superseded ones marked, because a correction never hides the original.
    pub fn inbox(&self, party: &PartyId) -> Vec<&Document> {
        let mut docs: Vec<&Document> = self
            .by_party
            .get(party)
            .into_iter()
            .flatten()
            .filter_map(|id| self.documents.get(id))
            .filter(|d| d.destroyed_on.is_none())
            .collect();
        docs.sort_by(|a, b| b.issued_at.cmp(&a.issued_at).then_with(|| b.id.cmp(&a.id)));
        docs
    }

    /// The sequence of schedules: the policy's documentary history.
    pub fn schedule_history(&self, policy: &PolicyId) -> Vec<&Document> {
        self.policy_documents(policy)
            .into_iter()
            .filter(|d| d.kind == DocumentKind::PolicySchedule)
            .collect()
    }

    pub fn policy_documents(&self, policy: &PolicyId) -> Vec<&Document> {
        self.by_policy
            .get(policy)
            .into_iter()
            .flatten()
            .filter_map(|id| self.documents.get(id))
            .collect()
    }

    /// May the thing a notice warns of take effect on a date?
    pub fn notice_gate(&self, policy: &PolicyId, purpose: &str, on: NaiveDate) -> Gate {
        let mut gate = Gate::NoNotice;
        for doc in self.policy_documents(policy) {
            let Some(notice) = doc.notice.as_ref().filter(|n| n.purpose == purpose) else {
                continue;
            };
            if doc.superseded_by.is_some() {
                continue;
            }
            gate = if notice.defect.is_some() {
                Gate::Waiting(Waiting::Defective)
            } else if notice.undeliverable {
                Gate::Waiting(Waiting::Undeliverable)
            } else {
                match notice.earliest_effect {
                    None => Gate::Waiting(Waiting::NotYetDelivered),
                    Some(date) if date > on => Gate::Waiting(Waiting::PeriodRunning { until: date }),
                    Some(date) => Gate::Clear { since: date },
                }
            };
        }
        gate
    }

    /// The proven notices rendered by a template version in a period: the
    /// population of a defect.
    pub fn notices_by_template(&self, template: &TemplateRef, from: NaiveDate, to: NaiveDate) -> Vec<String> {
        self.documents
            .values()
            .filter(|d| d.template.as_ref() == Some(template) && d.requires_proof() && d.issued_on >= from && d.issued_on <= to)
            .map(|d| d.id.clone())
            .collect()
    }

    /// The send a provider's reference belongs to, among the deliveries
    /// still in progress.
    pub fn find_send(&self, reference: &str) -> Option<PendingSend> {
        self.open_deliveries.iter().filter_map(|id| self.documents.get(id)).find_map(|doc| {
            doc.deliveries.iter().find_map(|d| {
                let attempt = d.attempts.iter().find(|a| a.provider_reference.as_deref() == Some(reference))?;
                Some(PendingSend {
                    document: doc.id.clone(),
                    party: d.recipient.party.clone(),
                    channel: attempt.channel,
                    kind: doc.kind,
                    format: doc.format,
                })
            })
        })
    }

    pub fn is_held(&self, document: &str) -> bool {
        self.documents.get(document).is_some_and(|d| self.held(d))
    }

    /// A hash of the whole state. Replicas that applied the same log agree.
    pub fn state_hash(&self) -> String {
        // Every map in the state has keys JSON can carry, so this cannot fail.
        sha256_hex(&serde_json::to_vec(self).expect("the state is serialisable"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSend {
    pub document: String,
    pub party: PartyId,
    pub channel: Channel,
    pub kind: DocumentKind,
    pub format: Option<crate::template::AlternateFormat>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting {
    NotYetDelivered,
    Undeliverable,
    Defective,
    PeriodRunning { until: NaiveDate },
}

/// Whether what a notice warns of may go ahead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gate {
    /// No notice of that purpose has been issued for the policy.
    NoNotice,
    Waiting(Waiting),
    Clear {
        since: NaiveDate,
    },
}

/// What the core's standing and status mean for the notices to send.
pub(crate) fn standing_notice(to: Option<Standing>) -> Option<(DocumentKind, &'static str)> {
    match to {
        // Entering grace is the warning that a lapse will follow.
        Some(Standing::InGrace) => Some((DocumentKind::LapseNotice, "lapse")),
        Some(Standing::Suspended) => Some((DocumentKind::SuspensionNotice, "suspension")),
        Some(Standing::Paid) | None => None,
    }
}

/// Whether a document the core's events require is copied to the parties
/// with an interest in the policy: the notices of arrears and suspension,
/// the letter that says the policy has ended, and the avoidance letter.
fn copied_to_interests(kind: DocumentKind) -> bool {
    matches!(
        kind,
        DocumentKind::LapseNotice | DocumentKind::SuspensionNotice | DocumentKind::GeneralCorrespondence | DocumentKind::AvoidanceNotice
    )
}

/// Maps whose keys are not plain strings are saved as lists of pairs,
/// because a JSON object key must be a string.
mod pairs {
    use std::collections::BTreeMap;

    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<K: Serialize, V: Serialize, S: Serializer>(map: &BTreeMap<K, V>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(map.iter())
    }

    pub fn deserialize<'de, K: DeserializeOwned + Ord, V: DeserializeOwned, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<K, V>, D::Error> {
        Ok(Vec::<(K, V)>::deserialize(d)?.into_iter().collect())
    }
}
