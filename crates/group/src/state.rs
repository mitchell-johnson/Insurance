//! The module's state and the handling of each command. A deterministic
//! state machine: the same commands in the same order give the same state on
//! every replica.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{Days, NaiveDate};
use ic_core::{GroupId, LifeId, PartyId, PolicyId, Reject};
use ic_log::StateMachine;
use ic_product::ProductDefinition;
use serde::{Deserialize, Serialize};

use crate::command::{Envelope, GroupCommand};
use crate::diff::{derive, trips_safety_check, Ambiguity, DiffContext};
use crate::event::{ExceptionKind, FileSummary, GroupEvent, Outbound, ReinsuranceNotice};
use crate::mapping::{FileKind, FileMapping, RowReject};
use crate::model::{ChangeKind, Membership, Proposed};
use crate::record::MemberRow;
use crate::terms::{ExcessLoading, MemberBasis, SchemeTerms};
use crate::work::{Item, Work};

/// Rows accepted in one `SubmitRows` command.
pub const MAX_ROWS_PER_PART: usize = 2_000;
/// Idempotency keys and closed files are kept this long.
pub const RETENTION_DAYS: u64 = 400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SchemeStatus {
    Active,
    Terminating { effective: NaiveDate },
    Terminated { effective: NaiveDate },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Stored, not yet scanned. Nothing has read it.
    Quarantined,
    Refused,
    Scanned,
    /// Held by the safety check until a person confirms it.
    Held,
    AwaitingApproval,
    AwaitingBackdatingDecision,
    ResolvingParties,
    Applying,
    Applied,
    Withdrawn,
}

impl FileStatus {
    /// Nothing further will happen to the file.
    pub fn is_closed(self) -> bool {
        matches!(self, FileStatus::Refused | FileStatus::Applied | FileStatus::Withdrawn)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub content_hash: String,
    pub status: FileStatus,
    pub received: NaiveDate,
    pub uploaded_by: String,
    #[serde(default)]
    pub approved_by: Option<String>,
    #[serde(default)]
    pub kind: Option<FileKind>,
    #[serde(default)]
    pub file_date: Option<NaiveDate>,
    /// Rows received so far, until the last part arrives.
    #[serde(default)]
    pub rows: Vec<MemberRow>,
    #[serde(default)]
    pub rejected: Vec<RowReject>,
    #[serde(default)]
    pub ambiguities: Vec<Ambiguity>,
    /// The changes the file would make, until they are queued.
    #[serde(default)]
    pub changes: Vec<Proposed>,
    #[serde(default)]
    pub summary: FileSummary,
    /// Joiners still waiting for the customer master's answer.
    #[serde(default)]
    pub unresolved: BTreeSet<String>,
    #[serde(default)]
    pub resolutions: BTreeMap<String, crate::command::Resolution>,
    /// Queued changes not yet applied or refused.
    #[serde(default)]
    pub remaining: u32,
    #[serde(default)]
    pub applied: u32,
    #[serde(default)]
    pub refused: u32,
    #[serde(default)]
    pub scheduled: u32,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub stale_flagged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Requested,
    /// Accepted by underwriting; the layer is being written to the core.
    Writing,
    Accepted,
    Declined,
    NotProceeded,
    Ended,
}

/// A member's cover above the automatic amount, from request to layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcessCase {
    pub id: u64,
    pub employee_id: String,
    pub life: LifeId,
    pub cover: String,
    /// What the formula gives above the automatic amount.
    pub excess_minor: i64,
    /// What underwriting accepted.
    pub accepted_minor: i64,
    pub status: CaseStatus,
    pub effective: NaiveDate,
    /// The core's layer, once its event has been seen.
    #[serde(default)]
    pub layer: Option<u32>,
    /// The layer is to end from this date as soon as its number is known.
    #[serde(default)]
    pub end_from: Option<NaiveDate>,
    #[serde(default)]
    pub ended_by_leave: bool,
    #[serde(default)]
    pub reference: Option<String>,
    /// The layer carries a loading that is billed to the employer inside
    /// the scheme's one total. Kept so the module can tell when too few
    /// members are left in that total; never put in an event.
    #[serde(default)]
    pub in_employer_total: bool,
    /// The case whose layer this one's layer takes the place of, where
    /// underwriting reissued the terms with the member paying the loading.
    /// The old layer ends when this one is in the core.
    #[serde(default)]
    pub replaces: Option<u64>,
}

impl ExcessCase {
    /// The request to underwriting this case's cover goes back to.
    pub fn request_case(&self) -> u64 {
        self.replaces.unwrap_or(self.id)
    }

    /// What this case counts for against the member's formula excess.
    pub fn counts_minor(&self) -> i64 {
        match self.status {
            CaseStatus::Requested => self.excess_minor,
            // A replacement counts once its own layer is written: until
            // then the layer it replaces is the cover held.
            CaseStatus::Writing if self.replaces.is_some() => 0,
            CaseStatus::Writing | CaseStatus::Accepted if self.end_from.is_none() => self.accepted_minor,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OfferStatus {
    Open,
    Issuing { policy: PolicyId, group: GroupId },
    TakenUp { policy: PolicyId },
    Expired,
}

/// A leaver's right to an individual policy without evidence of health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: u64,
    pub employee_id: String,
    pub life: LifeId,
    /// The first day without scheme cover: the new policy starts here.
    pub left: NaiveDate,
    pub expires: NaiveDate,
    /// The most that can be continued: the automatic cover at leaving.
    pub max_sum_insured_minor: i64,
    pub status: OfferStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renewal {
    pub opened: NaiveDate,
    /// Terms for the new year and the rate table agreed, once agreed.
    #[serde(default)]
    pub agreed: Option<(SchemeTerms, String)>,
    #[serde(default)]
    pub running_on_old_rates: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupLine {
    pub members: u32,
    pub total_minor: i64,
}

/// One scheme invoice as the employer sees it: a schedule by billing group,
/// retrospective adjustments since the last invoice, and one total for
/// loadings that are not shown against any member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invoice {
    pub period_index: u32,
    pub by_group: BTreeMap<String, GroupLine>,
    pub adjustments_by_group: BTreeMap<String, i64>,
    pub loading_minor: i64,
    pub loading_adjustments_minor: i64,
    /// The scheme terms version in force when it was issued, which says how
    /// loadings were billed.
    pub terms_version: u32,
    /// What the scheme still owed from before this invoice when it was
    /// issued: earlier invoices paid short or not at all. An overpayment
    /// shows as a negative amount. It is the scheme's, never a member's.
    #[serde(default)]
    pub brought_forward_minor: i64,
}

impl Invoice {
    /// What the scheme is asked to pay with this invoice: this period's
    /// charges and what was left unpaid before it.
    pub fn amount_due_minor(&self) -> i64 {
        self.total_minor() + self.brought_forward_minor
    }
    /// This period's charges.
    pub fn total_minor(&self) -> i64 {
        self.by_group.values().map(|g| g.total_minor).sum::<i64>()
            + self.adjustments_by_group.values().sum::<i64>()
            + self.loading_minor
            + self.loading_adjustments_minor
    }
    pub fn members(&self) -> u32 {
        self.by_group.values().map(|g| g.members).sum()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub key: String,
    /// The outbox entry carrying the command.
    pub outbox: u64,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scheme {
    pub id: String,
    pub policy: PolicyId,
    pub group: GroupId,
    pub employer: PartyId,
    pub inception: NaiveDate,
    pub product: ProductDefinition,
    pub terms: SchemeTerms,
    pub terms_version: u32,
    pub mapping: Option<FileMapping>,
    /// The layout for the other kind of file, where an employer sends
    /// both: a monthly full file and changes in between, say from payroll.
    #[serde(default)]
    pub second_mapping: Option<FileMapping>,
    pub status: SchemeStatus,
    /// The core refused something for a reason no file row explains.
    /// Nothing more is applied until staff resume the scheme.
    pub blocked: Option<String>,
    pub frozen: Option<String>,
    pub membership: Membership,
    pub files: BTreeMap<String, FileRecord>,
    /// The file being worked on. One at a time, so that every file is
    /// compared with membership as the file before it left it.
    pub open_file: Option<String>,
    pub queue: VecDeque<Item>,
    /// Changes dated in the future, released on their day.
    pub scheduled: BTreeMap<(NaiveDate, u64), Item>,
    pub next_item: u64,
    pub in_flight: Option<InFlight>,
    /// How many membership changes go to the core in one command. Halved
    /// when the core refuses a chunk, to find the row it refused.
    pub chunk_limit: usize,
    /// The core asked us to retry shortly. Cleared by the next tick.
    pub waiting: bool,
    pub core_seq: u64,
    pub cases: BTreeMap<u64, ExcessCase>,
    pub possible: BTreeMap<String, Vec<PartyId>>,
    pub offers: BTreeMap<u64, Offer>,
    pub pools: BTreeMap<String, NaiveDate>,
    pub declaration_due: Option<NaiveDate>,
    pub absence_leaves: BTreeSet<String>,
    pub renewal: Option<Renewal>,
    pub building: Option<Invoice>,
    pub pending_adjustments: BTreeMap<String, i64>,
    pub pending_loading_adjustments_minor: i64,
    pub invoices: VecDeque<Invoice>,
    pub outstanding_minor: i64,
    pub oldest_unpaid_due: Option<NaiveDate>,
    pub short_payments: u32,
    pub joiners_suspended: bool,
}

impl Scheme {
    pub fn can_take_files(&self) -> Result<(), Reject> {
        if let Some(reason) = &self.frozen {
            return Err(Reject::new("scheme_frozen", format!("files are on hold: {reason}")));
        }
        match self.status {
            SchemeStatus::Active => Ok(()),
            _ => Err(Reject::new("scheme_ending", "the scheme is ending and takes no more files")),
        }
    }

    pub(crate) fn file_mut(&mut self, file: &str, expected: &[FileStatus]) -> Result<&mut FileRecord, Reject> {
        file_in(&mut self.files, file, expected)
    }
}

/// A file of the scheme, if it is in one of the expected states.
/// What a file would do to a scheme as it stands: the changes, the rows
/// refused, and whether the safety check would hold it.
pub(crate) struct Compared {
    pub changes: Vec<Proposed>,
    pub rejected: Vec<RowReject>,
    pub ambiguities: Vec<Ambiguity>,
    pub summary: FileSummary,
    pub held: bool,
}

/// Compare a file's rows with the membership. Changes nothing: the same
/// comparison serves a real file and a trial of a new mapping.
pub(crate) fn compare(s: &Scheme, today: NaiveDate, kind: FileKind, file_date: NaiveDate, rows: &[MemberRow], mut rejected: Vec<RowReject>) -> Compared {
    let ctx = DiffContext {
        terms: &s.terms,
        product: &s.product,
        inception: s.inception,
        today,
        file_date,
    };
    let mut derived = derive(&ctx, &s.membership, kind, rows);
    // A scheme that keeps paying short takes no new joiners until it
    // has paid; its existing members are untouched.
    if s.joiners_suspended {
        let (joins, rest): (Vec<_>, Vec<_>) = derived.changes.into_iter().partition(|c| c.kind == ChangeKind::Join);
        derived.changes = rest;
        derived
            .rejected
            .extend(joins.iter().map(|c| RowReject::new(c.row, Some(&c.employee_id), "joiners_suspended", None)));
    }
    let members_before = s.membership.active_count();
    let leavers = derived.count(&ChangeKind::Leave);
    let above = derived
        .changes
        .iter()
        .filter(|c| matches!(c.kind, ChangeKind::Join | ChangeKind::Change))
        .filter(|c| {
            let cap = s.membership.active(&c.employee_id).and_then(|m| m.auto_cap_minor);
            crate::benefit::member_cover(&s.product, &c.attrs, cap).iter().any(|a| a.excess_minor() > 0)
        })
        .count();
    rejected.extend(derived.rejected);
    rejected.sort_by_key(|r| r.row);
    let count = |pick: &dyn Fn(&Proposed) -> bool| derived.changes.iter().filter(|c| pick(c)).count() as u32;
    let summary = FileSummary {
        rows: rows.len() as u32 + rejected.iter().filter(|r| !rows.iter().any(|row| row.row == r.row)).count() as u32,
        joiners: count(&|c| c.kind == ChangeKind::Join),
        leavers: leavers as u32,
        changes: count(&|c| c.kind == ChangeKind::Change),
        absences: count(&|c| matches!(c.kind, ChangeKind::AbsenceStart | ChangeKind::AbsenceEnd)),
        rekeys: count(&|c| matches!(c.kind, ChangeKind::Rekey { .. })),
        rejected: rejected.len() as u32,
        ambiguities: derived.ambiguities.len() as u32,
        needing_backdating_approval: count(&|c| c.needs_backdating_approval),
        above_automatic_cover: above as u32,
        members_before: members_before as u32,
    };
    Compared {
        changes: derived.changes,
        rejected,
        ambiguities: derived.ambiguities,
        summary,
        held: kind == FileKind::Full && trips_safety_check(&s.terms, members_before, leavers),
    }
}

/// The commission module's layer for each of a scheme's covers driven by
/// member data, written by its broker. Every such cover needs an expected
/// premium, and nothing else may be named.
fn broker_layers(
    scheme: &str,
    policy: &PolicyId,
    product: &ProductDefinition,
    broker: &crate::terms::SchemeBroker,
) -> Result<Vec<(String, crate::event::CommissionLayer)>, Reject> {
    let covers: Vec<&str> = crate::benefit::formula_covers(product).map(|c| c.code.as_str()).collect();
    if broker.expected_annual_premium_minor.keys().any(|c| !covers.contains(&c.as_str())) || covers.len() != broker.expected_annual_premium_minor.len() {
        return Err(Reject::new(
            "invalid_terms",
            format!("the broker's expected premium is given for exactly the scheme's covers: {}", covers.join(", ")),
        ));
    }
    Ok(covers
        .into_iter()
        .map(|cover| {
            let layer = crate::event::CommissionLayer {
                policy: policy.clone(),
                cover: cover.to_string(),
                engagement: broker.engagement.clone(),
                book: broker.book.clone(),
                schedule: broker.schedule.clone(),
                option: broker.option.clone(),
                annual_premium_minor: broker.expected_annual_premium_minor[cover],
            };
            (format!("grp-dist:{scheme}:{cover}"), layer)
        })
        .collect())
}

fn file_in<'a>(files: &'a mut BTreeMap<String, FileRecord>, file: &str, expected: &[FileStatus]) -> Result<&'a mut FileRecord, Reject> {
    let f = files
        .get_mut(file)
        .ok_or_else(|| Reject::new("unknown_file", format!("file {file} is not known for this scheme")))?;
    if !expected.contains(&f.status) {
        return Err(Reject::new("wrong_file_status", format!("file {file} is {:?}", f.status)));
    }
    Ok(f)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exception {
    pub scheme: String,
    pub kind: ExceptionKind,
    pub subject: String,
    pub raised: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Stored {
    on: NaiveDate,
    error: Option<Reject>,
}

/// What a command produced. Everything else it did is in the state: the
/// outbox holds what the driver must now do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub result: Result<(), Reject>,
    /// The key had been seen before: this is the original result.
    pub duplicate: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupState {
    pub schemes: BTreeMap<String, Scheme>,
    pub by_policy: BTreeMap<PolicyId, String>,
    idempotency: BTreeMap<String, Stored>,
    pub outbox: BTreeMap<u64, Outbound>,
    next_outbox: u64,
    next_event_seq: u64,
    next_id: u64,
    /// Commands sent to the core and not yet answered: key to scheme.
    pub core_keys: BTreeMap<String, String>,
    pub exceptions: BTreeMap<u64, Exception>,
    /// The business date of the last command, for effects raised without one.
    today: Option<NaiveDate>,
}

impl StateMachine for GroupState {
    type Command = Envelope;
    type Output = Outcome;
    fn apply(&mut self, _index: u64, command: Envelope) -> Outcome {
        self.apply_envelope(command)
    }
}

impl GroupState {
    pub fn apply_envelope(&mut self, env: Envelope) -> Outcome {
        if let Some(stored) = self.idempotency.get(&env.key) {
            return Outcome {
                result: stored.error.clone().map_or(Ok(()), Err),
                duplicate: true,
            };
        }
        let result = if env.command.allowed_callers().contains(&env.caller.as_str()) {
            // Every handler checks before it writes, so a refused command
            // leaves no trace.
            self.today = Some(env.today);
            self.dispatch(&env.actor, env.today, env.command)
        } else {
            Err(Reject::new("caller_not_allowed", format!("{} may not send {}", env.caller, env.command.name())))
        };
        self.idempotency.insert(
            env.key,
            Stored {
                on: env.today,
                error: result.clone().err(),
            },
        );
        Outcome { result, duplicate: false }
    }

    /// The business date of the last command applied.
    pub fn business_date(&self) -> Option<NaiveDate> {
        self.today
    }

    pub(crate) fn fresh_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub(crate) fn send(&mut self, out: Outbound) -> u64 {
        self.next_outbox += 1;
        self.outbox.insert(self.next_outbox, out);
        self.next_outbox
    }

    pub(crate) fn emit(&mut self, event: GroupEvent) {
        self.next_event_seq += 1;
        let (seq, date) = (self.next_event_seq, self.today.unwrap_or(NaiveDate::MIN));
        self.send(Outbound::Publish { seq, date, event });
    }

    pub(crate) fn raise(&mut self, scheme: &str, kind: ExceptionKind, subject: &str) -> u64 {
        let existing = self
            .exceptions
            .iter()
            .find(|(_, e)| e.scheme == scheme && e.kind == kind && e.subject == subject);
        if let Some((id, _)) = existing {
            return *id;
        }
        let id = self.fresh_id();
        self.exceptions.insert(
            id,
            Exception {
                scheme: scheme.to_string(),
                kind,
                subject: subject.to_string(),
                raised: self.today.unwrap_or(NaiveDate::MIN),
            },
        );
        self.emit(GroupEvent::ExceptionRaised {
            id,
            scheme: scheme.to_string(),
            kind,
            subject: subject.to_string(),
        });
        id
    }

    pub(crate) fn clear(&mut self, scheme: &str, kind: ExceptionKind, subject: Option<&str>) {
        let ids: Vec<u64> = self
            .exceptions
            .iter()
            .filter(|(_, e)| e.scheme == scheme && e.kind == kind && subject.is_none_or(|s| e.subject == s))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.exceptions.remove(&id);
            self.emit(GroupEvent::ExceptionCleared {
                id,
                scheme: scheme.to_string(),
                kind,
            });
        }
    }

    pub(crate) fn notify_reinsurance(&mut self, notice: ReinsuranceNotice) {
        let key = format!("grp-ri:{}", self.next_outbox + 1);
        self.send(Outbound::Reinsurance { key, notice });
    }

    /// Take a scheme out, work on it with the rest of the state to hand,
    /// and put it back.
    pub(crate) fn with_scheme<T>(&mut self, id: &str, f: impl FnOnce(&mut Self, &mut Scheme) -> Result<T, Reject>) -> Result<T, Reject> {
        let mut scheme = self
            .schemes
            .remove(id)
            .ok_or_else(|| Reject::new("unknown_scheme", format!("scheme {id} is not registered")))?;
        let r = f(self, &mut scheme);
        if r.is_ok() {
            self.check_loading_total(&scheme);
        }
        self.schemes.insert(id.to_string(), scheme);
        r
    }

    /// Whether the employer's one total of loadings still hides whose they
    /// are. The employer knows which members hold underwritten cover, so a
    /// total is safe only while at least the scheme's minimum count of
    /// members hold it. Who pays a loading is settled when terms are
    /// accepted and is what the member was told, so it is not changed
    /// here: when leavers or ended cover take the holders below the count
    /// while a loading is still in the total, staff are told, and told
    /// that it is over when the number recovers or the total is empty.
    fn check_loading_total(&mut self, s: &Scheme) {
        let open = |c: &&ExcessCase| c.end_from.is_none() && matches!(c.status, CaseStatus::Writing | CaseStatus::Accepted);
        let holders: BTreeSet<&str> = s.cases.values().filter(open).map(|c| c.employee_id.as_str()).collect();
        let in_total = s.cases.values().filter(open).any(|c| c.in_employer_total);
        let counted = s.terms.excess_loading == ExcessLoading::EmployerTotal;
        if counted && in_total && (holders.len() as u32) < s.terms.excess_loading_min_count {
            self.raise(&s.id, ExceptionKind::LoadingTotalTooSmall, "loadings");
        } else {
            self.clear(&s.id, ExceptionKind::LoadingTotalTooSmall, None);
        }
    }

    fn dispatch(&mut self, actor: &str, today: NaiveDate, command: GroupCommand) -> Result<(), Reject> {
        use GroupCommand::*;
        match command {
            RegisterScheme {
                scheme,
                policy,
                group,
                employer,
                inception,
                product,
                terms,
                founding,
                pools,
            } => self.register(scheme, policy, group, employer, inception, *product, *terms, founding, pools),
            AmendTerms { scheme, terms } => self.with_scheme(&scheme, |st, s| {
                terms.validate().map_err(|e| Reject::new("invalid_terms", e))?;
                // A broker is added once. Changing or removing one is a dated
                // move of the scheme's book, made in the commission module.
                let added = match (&s.terms.broker, &terms.broker) {
                    (None, Some(b)) => Some(broker_layers(&s.id, &s.policy, &s.product, b)?),
                    (old, new) if old == new => None,
                    _ => {
                        return Err(Reject::new(
                            "broker_change",
                            "a scheme's broker is changed or removed by a dated move in the commission module",
                        ))
                    }
                };
                for (key, layer) in added.into_iter().flatten() {
                    st.send(Outbound::Commission { key, layer });
                }
                s.terms = *terms;
                s.terms_version += 1;
                st.emit(GroupEvent::TermsAmended {
                    scheme: s.id.clone(),
                    terms_version: s.terms_version,
                });
                st.notify_reinsurance(ReinsuranceNotice::SetSchemeTerms {
                    policy: s.policy.clone(),
                    excess_path: s.terms.excess_path,
                    sites: s.terms.sites.clone(),
                });
                Ok(())
            }),
            SetMapping { scheme, mapping } => self.with_scheme(&scheme, |st, s| {
                if !mapping.is_sealed() {
                    return Err(Reject::new("mapping_not_sealed", "the mapping's content hash does not match its content"));
                }
                // One layout for full files and one for change files, each
                // replaced only by a later version of itself.
                let first = s.mapping.as_ref().is_none_or(|m| m.kind == mapping.kind);
                let held = if first { &s.mapping } else { &s.second_mapping };
                if held.as_ref().is_some_and(|m| m.version >= mapping.version) {
                    return Err(Reject::new("mapping_version", "a mapping is replaced only by a later version"));
                }
                st.emit(GroupEvent::MappingSet {
                    scheme: s.id.clone(),
                    mapping: mapping.id.clone(),
                    version: mapping.version,
                    content_hash: mapping.content_hash.clone(),
                });
                if first {
                    s.mapping = Some(*mapping);
                } else {
                    s.second_mapping = Some(*mapping);
                }
                Ok(())
            }),
            Freeze { scheme, reason } => self.with_scheme(&scheme, |st, s| {
                s.frozen = Some(reason.clone());
                st.emit(GroupEvent::SchemeFrozen { scheme: s.id.clone(), reason });
                Ok(())
            }),
            Unfreeze { scheme } => self.with_scheme(&scheme, |st, s| {
                if s.frozen.take().is_none() {
                    return Err(Reject::new("not_frozen", "the scheme is not frozen"));
                }
                st.emit(GroupEvent::SchemeUnfrozen { scheme: s.id.clone() });
                Ok(())
            }),
            Resume { scheme } => self.with_scheme(&scheme, |st, s| {
                if s.blocked.take().is_none() {
                    return Err(Reject::new("not_blocked", "the scheme is not blocked"));
                }
                st.clear(&s.id, ExceptionKind::SchemeBlocked, None);
                st.emit(GroupEvent::SchemeResumed { scheme: s.id.clone() });
                st.pump(s);
                Ok(())
            }),
            ReceiveFile { scheme, file, content_hash } => self.with_scheme(&scheme, |st, s| st.receive_file(s, actor, today, file, content_hash)),
            RecordScan { scheme, file, clean } => self.with_scheme(&scheme, |st, s| {
                let f = s.file_mut(&file, &[FileStatus::Quarantined])?;
                if clean {
                    f.status = FileStatus::Scanned;
                    st.emit(GroupEvent::FileScanned { scheme: s.id.clone(), file });
                } else {
                    st.refuse_file(s, &file, "malware");
                }
                Ok(())
            }),
            RefuseFile { scheme, file, code } => self.with_scheme(&scheme, |st, s| {
                s.file_mut(&file, &[FileStatus::Quarantined, FileStatus::Scanned])?;
                st.refuse_file(s, &file, &code);
                Ok(())
            }),
            SubmitRows {
                scheme,
                file,
                mapping_hash,
                kind,
                file_date,
                rows,
                rejected,
                last,
            } => self.with_scheme(&scheme, |st, s| {
                st.submit_rows(s, today, &file, &mapping_hash, kind, file_date, rows, rejected, last)
            }),
            ConfirmHeld { scheme, file } => self.with_scheme(&scheme, |st, s| {
                s.file_mut(&file, &[FileStatus::Held])?.status = FileStatus::AwaitingApproval;
                st.clear(&s.id, ExceptionKind::FileHeld, Some(&file));
                st.emit(GroupEvent::FileAwaitingApproval { scheme: s.id.clone(), file });
                Ok(())
            }),
            ApproveFile { scheme, file } => self.with_scheme(&scheme, |st, s| st.approve_file(s, actor, today, &file)),
            DecideBackdating { scheme, file, approve } => self.with_scheme(&scheme, |st, s| {
                let f = s.file_mut(&file, &[FileStatus::AwaitingBackdatingDecision])?;
                if approve {
                    f.changes.iter_mut().for_each(|c| c.needs_backdating_approval = false);
                } else {
                    let (keep, drop): (Vec<_>, Vec<_>) = std::mem::take(&mut f.changes).into_iter().partition(|c| !c.needs_backdating_approval);
                    f.changes = keep;
                    for c in drop {
                        f.rejected
                            .push(RowReject::new(c.row, Some(&c.employee_id), "backdating_declined", Some("effective")));
                        f.refused += 1;
                    }
                }
                st.start_resolving(s, today, &file);
                Ok(())
            }),
            WithdrawFile { scheme, file } => self.with_scheme(&scheme, |st, s| {
                let open = [
                    FileStatus::Quarantined,
                    FileStatus::Scanned,
                    FileStatus::Held,
                    FileStatus::AwaitingApproval,
                    FileStatus::AwaitingBackdatingDecision,
                ];
                let f = s.file_mut(&file, &open)?;
                f.status = FileStatus::Withdrawn;
                f.rows.clear();
                f.changes.clear();
                st.close_file(s, &file);
                st.emit(GroupEvent::FileWithdrawn { scheme: s.id.clone(), file });
                Ok(())
            }),
            PartiesResolved {
                scheme,
                file,
                resolutions,
                last,
            } => self.with_scheme(&scheme, |st, s| st.parties_resolved(s, today, &file, resolutions, last)),
            other => self.dispatch_work(actor, today, other),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn register(
        &mut self,
        id: String,
        policy: PolicyId,
        group: GroupId,
        employer: PartyId,
        inception: NaiveDate,
        product: ProductDefinition,
        terms: SchemeTerms,
        founding: Vec<crate::model::Member>,
        pools: Vec<String>,
    ) -> Result<(), Reject> {
        if id.is_empty() || id.contains(':') {
            return Err(Reject::new("invalid_scheme", "a scheme ID is not empty and has no colon"));
        }
        if self.schemes.contains_key(&id) || self.by_policy.contains_key(&policy) {
            return Err(Reject::new("scheme_exists", "the scheme or its policy is already registered"));
        }
        terms.validate().map_err(|e| Reject::new("invalid_terms", e))?;
        if crate::benefit::formula_covers(&product).next().is_none() {
            return Err(Reject::new("not_a_scheme", "the product has no cover driven by member data"));
        }
        let mut membership = Membership::default();
        for member in founding {
            if membership.members.contains_key(&member.employee_id) || membership.by_life(&member.life).is_some() {
                return Err(Reject::new("duplicate_member", format!("member {} is named twice", member.employee_id)));
            }
            membership.insert(member);
        }
        let pooled = terms.basis == MemberBasis::Pool;
        if pooled != membership.members.is_empty() || (!pooled && !pools.is_empty()) {
            return Err(Reject::new(
                "invalid_scheme",
                "a pool scheme has pools and no members; any other has members and no pools",
            ));
        }
        // A pool scheme owes its first declaration from the start.
        let declaration_due = pooled
            .then(|| terms.scheme_year_start.checked_add_days(Days::new(u64::from(terms.declaration_due_days))))
            .flatten();
        let pools: BTreeMap<String, NaiveDate> = pools.into_iter().map(|p| (p, inception)).collect();
        self.emit(GroupEvent::SchemeRegistered {
            scheme: id.clone(),
            policy: policy.clone(),
            terms_version: 1,
        });
        self.notify_reinsurance(ReinsuranceNotice::SetSchemeTerms {
            policy: policy.clone(),
            excess_path: terms.excess_path,
            sites: terms.sites.clone(),
        });
        if let Some(broker) = &terms.broker {
            let covers = broker_layers(&id, &policy, &product, broker)?;
            for (key, layer) in covers {
                self.send(Outbound::Commission { key, layer });
            }
        }
        self.by_policy.insert(policy.clone(), id.clone());
        self.schemes.insert(
            id.clone(),
            Scheme {
                id,
                policy,
                group,
                employer,
                inception,
                product,
                terms,
                terms_version: 1,
                mapping: None,
                second_mapping: None,
                status: SchemeStatus::Active,
                blocked: None,
                frozen: None,
                membership,
                files: BTreeMap::new(),
                open_file: None,
                queue: VecDeque::new(),
                scheduled: BTreeMap::new(),
                next_item: 0,
                in_flight: None,
                chunk_limit: ic_core::machine::MAX_MEMBERSHIP_CHUNK,
                waiting: false,
                core_seq: 0,
                cases: BTreeMap::new(),
                possible: BTreeMap::new(),
                offers: BTreeMap::new(),
                pools,
                declaration_due,
                absence_leaves: BTreeSet::new(),
                renewal: None,
                building: None,
                pending_adjustments: BTreeMap::new(),
                pending_loading_adjustments_minor: 0,
                invoices: VecDeque::new(),
                outstanding_minor: 0,
                oldest_unpaid_due: None,
                short_payments: 0,
                joiners_suspended: false,
            },
        );
        Ok(())
    }

    fn receive_file(&mut self, s: &mut Scheme, actor: &str, today: NaiveDate, file: String, content_hash: String) -> Result<(), Reject> {
        s.can_take_files()?;
        if s.terms.basis == MemberBasis::Pool {
            return Err(Reject::new("pool_scheme", "a pool scheme declares aggregates; it has no membership files"));
        }
        if s.files.contains_key(&file) {
            return Err(Reject::new("file_exists", format!("file {file} was already received")));
        }
        // The same content again is the same file: nothing to do, and the
        // caller is told which file it was.
        if let Some(same) = s
            .files
            .values()
            .find(|f| f.content_hash == content_hash && f.status != FileStatus::Refused && f.status != FileStatus::Withdrawn)
        {
            return Err(Reject::new("duplicate_file", format!("the same content was received as file {}", same.id)));
        }
        if let Some(open) = &s.open_file {
            return Err(Reject::new("file_in_progress", format!("file {open} is still being worked on")));
        }
        s.open_file = Some(file.clone());
        s.files.insert(
            file.clone(),
            FileRecord {
                id: file.clone(),
                content_hash,
                status: FileStatus::Quarantined,
                received: today,
                uploaded_by: actor.to_string(),
                approved_by: None,
                kind: None,
                file_date: None,
                rows: vec![],
                rejected: vec![],
                ambiguities: vec![],
                changes: vec![],
                summary: FileSummary::default(),
                unresolved: BTreeSet::new(),
                resolutions: BTreeMap::new(),
                remaining: 0,
                applied: 0,
                refused: 0,
                scheduled: 0,
                code: None,
                stale_flagged: false,
            },
        );
        self.emit(GroupEvent::FileQuarantined { scheme: s.id.clone(), file });
        Ok(())
    }

    fn refuse_file(&mut self, s: &mut Scheme, file: &str, code: &str) {
        if let Some(f) = s.files.get_mut(file) {
            f.status = FileStatus::Refused;
            f.code = Some(code.to_string());
            f.rows.clear();
        }
        self.close_file(s, file);
        self.emit(GroupEvent::FileRefused {
            scheme: s.id.clone(),
            file: file.to_string(),
            code: code.to_string(),
        });
    }

    /// The file is finished with: the next one may start.
    pub(crate) fn close_file(&mut self, s: &mut Scheme, file: &str) {
        if s.open_file.as_deref() == Some(file) {
            s.open_file = None;
        }
        self.clear(&s.id, ExceptionKind::FileHeld, Some(file));
        self.clear(&s.id, ExceptionKind::FileStale, Some(file));
    }

    #[allow(clippy::too_many_arguments)]
    fn submit_rows(
        &mut self,
        s: &mut Scheme,
        today: NaiveDate,
        file: &str,
        mapping_hash: &str,
        kind: FileKind,
        file_date: NaiveDate,
        rows: Vec<MemberRow>,
        rejected: Vec<RowReject>,
        last: bool,
    ) -> Result<(), Reject> {
        if rows.len() > MAX_ROWS_PER_PART {
            return Err(Reject::new("too_many_rows", format!("at most {MAX_ROWS_PER_PART} rows per part")));
        }
        let current = |m: &Option<FileMapping>| m.as_ref().is_some_and(|m| m.content_hash == mapping_hash);
        if !current(&s.mapping) && !current(&s.second_mapping) {
            return Err(Reject::new("mapping_mismatch", "the rows were not mapped with the scheme's current mapping"));
        }
        if file_date > today {
            return Err(Reject::new("file_date", "a file cannot speak for a date in the future"));
        }
        let f = s.file_mut(file, &[FileStatus::Scanned])?;
        f.kind = Some(kind);
        f.file_date = Some(file_date);
        f.rows.extend(rows);
        f.rejected.extend(rejected);
        if !last {
            return Ok(());
        }
        let rows = std::mem::take(&mut f.rows);
        let mapper_rejected = std::mem::take(&mut f.rejected);
        let compared = compare(s, today, kind, file_date, &rows, mapper_rejected);
        let (members_before, leavers, held) = (compared.summary.members_before, compared.summary.leavers, compared.held);
        let f = file_in(&mut s.files, file, &[FileStatus::Scanned])?;
        f.rejected = compared.rejected;
        f.ambiguities = compared.ambiguities;
        f.summary = compared.summary;
        f.changes = compared.changes;
        f.status = if held { FileStatus::Held } else { FileStatus::AwaitingApproval };
        self.emit(GroupEvent::FileCompared {
            scheme: s.id.clone(),
            file: file.to_string(),
            summary: f.summary.clone(),
            rejected: f.rejected.clone(),
            ambiguities: f.ambiguities.clone(),
        });
        if held {
            self.raise(&s.id, ExceptionKind::FileHeld, file);
            self.emit(GroupEvent::FileHeld {
                scheme: s.id.clone(),
                file: file.to_string(),
                leavers,
                members: members_before,
            });
        } else {
            self.emit(GroupEvent::FileAwaitingApproval {
                scheme: s.id.clone(),
                file: file.to_string(),
            });
        }
        Ok(())
    }

    fn approve_file(&mut self, s: &mut Scheme, actor: &str, today: NaiveDate, file: &str) -> Result<(), Reject> {
        s.can_take_files()?;
        let second = s.terms.second_approver;
        let f = file_in(&mut s.files, file, &[FileStatus::AwaitingApproval])?;
        if second && f.uploaded_by == actor {
            return Err(Reject::new("second_approver", "this scheme needs approval by someone other than the uploader"));
        }
        f.approved_by = Some(actor.to_string());
        let backdated = f.changes.iter().filter(|c| c.needs_backdating_approval).count() as u32;
        self.emit(GroupEvent::FileApproved {
            scheme: s.id.clone(),
            file: file.to_string(),
            by: actor.to_string(),
        });
        if backdated > 0 {
            f.status = FileStatus::AwaitingBackdatingDecision;
            self.emit(GroupEvent::FileAwaitingBackdatingDecision {
                scheme: s.id.clone(),
                file: file.to_string(),
                changes: backdated,
            });
            return Ok(());
        }
        self.start_resolving(s, today, file);
        Ok(())
    }

    /// Ask the customer master who the joiners are, or go straight to
    /// applying where there is nobody to ask about.
    fn start_resolving(&mut self, s: &mut Scheme, today: NaiveDate, file: &str) {
        let named = s.terms.basis == MemberBasis::Named;
        let Some(f) = s.files.get_mut(file) else { return };
        let joiners: Vec<String> = f
            .changes
            .iter()
            .filter(|c| named && c.kind == ChangeKind::Join)
            .map(|c| c.employee_id.clone())
            .collect();
        if joiners.is_empty() {
            self.queue_file(s, today, file);
            return;
        }
        f.status = FileStatus::ResolvingParties;
        f.unresolved = joiners.iter().cloned().collect();
        self.send(Outbound::ResolveParties {
            scheme: s.id.clone(),
            file: file.to_string(),
            employee_ids: joiners,
        });
    }

    fn parties_resolved(
        &mut self,
        s: &mut Scheme,
        today: NaiveDate,
        file: &str,
        resolutions: Vec<(String, crate::command::Resolution)>,
        last: bool,
    ) -> Result<(), Reject> {
        let f = s.file_mut(file, &[FileStatus::ResolvingParties])?;
        let mut waiting = f.unresolved.clone();
        if let Some((unknown, _)) = resolutions.iter().find(|(id, _)| !waiting.remove(id)) {
            return Err(Reject::new("not_waiting", format!("no joiner {unknown} is waiting to be resolved")));
        }
        if last && !waiting.is_empty() {
            return Err(Reject::new("unresolved_joiners", format!("{} joiners have no answer", waiting.len())));
        }
        f.unresolved = waiting;
        f.resolutions.extend(resolutions);
        if last {
            self.queue_file(s, today, file);
        }
        Ok(())
    }

    /// Turn an approved file's changes into queued work, in the file's
    /// order, holding back anything dated in the future.
    fn queue_file(&mut self, s: &mut Scheme, today: NaiveDate, file: &str) {
        let Some(f) = s.files.get_mut(file) else { return };
        f.status = FileStatus::Applying;
        let changes = std::mem::take(&mut f.changes);
        let mut resolutions = std::mem::take(&mut f.resolutions);
        f.remaining = changes.len() as u32;
        let mut scheduled = 0;
        for change in changes {
            let resolution = resolutions.remove(&change.employee_id).filter(|_| change.kind == ChangeKind::Join);
            let due = change.effective;
            s.next_item += 1;
            let item = Item {
                id: s.next_item,
                file: Some(file.to_string()),
                work: Work::Member {
                    change,
                    resolution,
                    life: None,
                },
            };
            if due > today {
                scheduled += 1;
                s.scheduled.insert((due, item.id), item);
            } else {
                s.queue.push_back(item);
            }
        }
        if let Some(f) = s.files.get_mut(file) {
            f.scheduled = scheduled;
            f.remaining -= scheduled;
        }
        self.finish_file_if_done(s, file);
        self.pump(s);
    }

    /// A file is applied once every change in it has been applied, refused
    /// or scheduled for its day.
    pub(crate) fn finish_file_if_done(&mut self, s: &mut Scheme, file: &str) {
        let Some(f) = s.files.get_mut(file).filter(|f| f.status == FileStatus::Applying && f.remaining == 0) else {
            return;
        };
        f.status = FileStatus::Applied;
        let (applied, refused, scheduled) = (f.applied, f.refused, f.scheduled);
        self.close_file(s, file);
        self.emit(GroupEvent::FileApplied {
            scheme: s.id.clone(),
            file: file.to_string(),
            applied,
            refused,
            scheduled,
        });
    }

    /// Forget what no longer matters: old idempotency keys and files closed
    /// long ago.
    pub(crate) fn prune(&mut self, today: NaiveDate) {
        let Some(cutoff) = today.checked_sub_days(Days::new(RETENTION_DAYS)) else {
            return;
        };
        self.idempotency.retain(|_, s| s.on >= cutoff);
        for s in self.schemes.values_mut() {
            s.files.retain(|_, f| !f.status.is_closed() || f.received >= cutoff);
        }
    }
}
