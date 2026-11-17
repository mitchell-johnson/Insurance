//! Views of the claims state, each cut for its audience.
//!
//! The claimant sees their own claim. An adviser or an employer sees status,
//! never cause. Cause-of-claim analysis exists only in [`Book`], which stays
//! inside the claims module.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::config::Queue;
use crate::model::{ClaimType, DecisionKind, RecoveryStatus, RunStatus, Status, TaskKind, TimerKind};
use crate::state::ClaimsState;
use crate::{ClaimId, StaffId};

/// What an adviser or an employer may see: that a claim exists and where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusView {
    pub claim: ClaimId,
    pub status: Status,
    pub notified_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
}

/// The claimant's own view of their claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimantView {
    pub claim: ClaimId,
    pub claim_type: ClaimType,
    pub status: Status,
    /// One owner at a time, visible to the claimant.
    pub owner: Option<StaffId>,
    pub notified_on: NaiveDate,
    pub decision_due: Option<NaiveDate>,
    /// Evidence still awaited: its ID, what it is and when it is due.
    pub evidence_outstanding: Vec<(String, String, NaiveDate)>,
    pub admitted_minor: i64,
    pub paid_minor: i64,
    pub next_benefit: Option<NaiveDate>,
    /// Only what is actually being asked for. A held overpayment asks
    /// nothing of the claimant, so it is not shown as owing.
    pub repayment_asked_minor: i64,
    /// Each benefit period paid, with every figure that made it up.
    pub benefit_lines: Vec<BenefitLine>,
}

/// One benefit period as the claimant's statement shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenefitLine {
    pub start: NaiveDate,
    pub end: NaiveDate,
    /// The benefit before other income.
    pub benefit_minor: i64,
    /// Other income declared for the period, taken off the benefit.
    pub other_income_minor: i64,
    /// A shortfall on earlier periods paid with this one.
    pub arrears_minor: i64,
    /// Taken back against an overpayment.
    pub overpayment_offset_minor: i64,
    /// What was paid.
    pub paid_minor: i64,
    pub returned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueRow {
    pub claim: ClaimId,
    pub priority: u32,
    pub age_days: i64,
    pub owner: Option<StaffId>,
}

/// Claims in payment as a portfolio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Book {
    pub in_payment: u32,
    pub monthly_minor: i64,
    pub reserve_minor: i64,
    /// Months in payment so far: `0-6`, `6-12`, `12-24`, `24+`.
    pub by_duration: BTreeMap<String, u32>,
    /// By root of the cause tag tree. Never leaves the claims module.
    pub by_cause: BTreeMap<String, u32>,
    pub expected_end_by_year: BTreeMap<i32, u32>,
    pub reviews_open: u32,
    pub reviews_overdue: u32,
    pub return_to_work: BTreeMap<String, u32>,
    pub caseload: BTreeMap<StaffId, u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Report {
    pub notified: BTreeMap<ClaimType, u32>,
    /// Straight-through admissions, by health benefit category or claim type.
    pub straight_through: BTreeMap<String, u32>,
    pub decisions: BTreeMap<DecisionKind, u32>,
    /// Decisions by the person who proposed them, for consistency reporting.
    pub by_assessor: BTreeMap<StaffId, BTreeMap<DecisionKind, u32>>,
    pub partial_admissions: u32,
    /// Declines overturned at internal review or by the dispute scheme.
    pub overturns: u32,
    /// The insurer overturning its own admission, reported apart from declines.
    pub admissions_reversed: u32,
    pub days_to_decision: i64,
    pub decisions_timed: u32,
    /// Service timers met on time and missed, by kind.
    pub timers: BTreeMap<TimerKind, (u32, u32)>,
    pub overpaid_minor: i64,
    pub recovered_minor: i64,
    pub written_off_minor: i64,
    pub held_minor: i64,
    pub evidence_cost_minor: i64,
    /// Late payment interest the payees were told of and declined.
    #[serde(default)]
    pub interest_declined_minor: i64,
    /// Resident withholding tax kept back from late payment interest, owed to the revenue authority.
    #[serde(default)]
    pub interest_withheld_minor: i64,
    /// Tax kept back from income benefit periods paid.
    #[serde(default)]
    pub benefit_tax_withheld_minor: i64,
    pub workload: BTreeMap<StaffId, u32>,
    pub runs_unreconciled: u32,
}

/// The figures the daily control compares with the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Totals {
    pub reserve_minor: i64,
    pub payable_minor: i64,
    pub receivable_minor: i64,
}

/// A question about the claims on a partition. Answering changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Question {
    /// One claim, as its claimant may see it.
    Claim {
        claim: ClaimId,
    },
    /// A work queue's open claims.
    Queue {
        queue: Queue,
        /// The person asking. A claim they may not work because of a
        /// declared relationship or because they underwrote the life is
        /// left out, so it is never offered to them. Absent: the whole queue.
        #[serde(default)]
        staff: Option<StaffId>,
    },
    /// The claims in payment as a portfolio. Carries cause analysis: for the
    /// claims module's own staff only.
    Book,
    Report,
    Remittance {
        run: String,
    },
    /// One claim as the people working it need it, without the clinical file.
    Work {
        claim: ClaimId,
        /// The person asking: a claim they are conflicted on is not shown.
        #[serde(default)]
        staff: Option<StaffId>,
    },
    /// The claims on one policy as an adviser or an employer may see them:
    /// that each exists and where it is, never its cause or type. The caller
    /// checks that the person asking serves or administers the policy.
    PolicyStatus {
        policy: String,
        /// A member of staff asking: a claim they are related to, or
        /// underwrote the life of, is not shown, not even that it exists.
        #[serde(default)]
        staff: Option<StaffId>,
    },
    /// The restricted cases, for an investigator. The caller puts the
    /// signed-in person here; anyone without the role is answered with none.
    Cases {
        staff: StaffId,
    },
    /// Letters asking a payee to repay that the documents service has not
    /// taken yet, oldest first. Only while repayment is still being asked.
    RepaymentLetters,
}

/// A repayment letter for the documents service to issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LetterDue {
    pub request: String,
    pub claim: ClaimId,
    pub policy: String,
    /// Who owes it, as `overpayment_raised` names them.
    pub party: String,
    pub life: Option<String>,
    pub overpayment: String,
    pub amount_minor: i64,
    pub asked_on: NaiveDate,
}

/// One claim for the staff working it: who has it, what is waiting and
/// what is owed. No cause, reason code, exclusion outcome or vault
/// reference is in it; those are read by opening the file, which is logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkView {
    pub claim: ClaimId,
    pub policy: String,
    pub cover: String,
    /// The life claimed on, when the policy has more than one.
    #[serde(default)]
    pub life: Option<String>,
    pub claim_type: ClaimType,
    pub status: Status,
    pub queue: Queue,
    pub owner: Option<StaffId>,
    /// The claimant has a communication marker: one named owner, handovers said, longer deadlines.
    #[serde(default)]
    pub communication_support: bool,
    pub event_date: NaiveDate,
    pub notified_on: NaiveDate,
    pub closed_on: Option<NaiveDate>,
    pub reserve_minor: i64,
    pub admitted_minor: i64,
    /// Admitted and not yet released, the claimant's part and any handling cost that came back.
    pub payable_minor: i64,
    pub paid_minor: i64,
    pub in_run: Option<String>,
    pub tasks: Vec<WorkTask>,
    /// Evidence asked for: its ID, what it is, who from, when due and when received.
    pub evidence: Vec<(String, String, String, NaiveDate, Option<NaiveDate>)>,
    pub waiting: Option<WorkDecision>,
    /// The verified payee: the party and what kind of payee they are.
    pub payee: Option<(String, crate::PayeeKind)>,
    /// Interested parties and what each is still owed before the claimant.
    pub interests: Vec<(String, i64)>,
    /// Payments: reference, amount, date, returned.
    pub payments: Vec<(String, i64, NaiveDate, bool)>,
    pub benefit: Option<WorkBenefit>,
    /// Overpayments: ID, outstanding and recovery status.
    pub overpayments: Vec<(String, i64, RecoveryStatus)>,
    /// Service timers: due, met and how far escalated.
    pub timers: BTreeMap<TimerKind, (NaiveDate, Option<NaiveDate>, u8)>,
    pub waiver_from: Option<NaiveDate>,
    /// On a continuation policy, the scheme it carries on from and the
    /// member's claims there: ID, type, status and event date.
    pub continued_from: Option<crate::port::Continuation>,
    pub scheme_claims: Vec<(ClaimId, ClaimType, Status, NaiveDate)>,
    /// The wordings the cover is bound to, as `id@version`, from the cover
    /// check. Empty until the cover is checked.
    #[serde(default)]
    pub definitions: Vec<String>,
    /// The wordings each decision relied on, as `id@version`, by decision
    /// number: what the assessor read when deciding.
    #[serde(default)]
    pub decision_definitions: BTreeMap<u32, Vec<String>>,
    /// The cover on the event date as known now and as known then, where
    /// something recorded later changed it: (in force, life on the policy,
    /// sum insured) each.
    #[serde(default)]
    pub cover_now_and_then: Option<((bool, bool, i64), (bool, bool, i64))>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkTask {
    pub id: String,
    pub kind: TaskKind,
    pub due: Option<NaiveDate>,
    pub completed_on: Option<NaiveDate>,
    pub overdue: bool,
}

/// A decision proposed and waiting for its signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDecision {
    pub kind: DecisionKind,
    pub proposer: StaffId,
    pub proposed_on: NaiveDate,
    pub signed: Vec<StaffId>,
    /// How many more signatures it needs.
    pub still_needed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkBenefit {
    pub monthly_minor: i64,
    pub next_period: NaiveDate,
    pub end: NaiveDate,
    pub reviews: BTreeMap<TaskKind, NaiveDate>,
    pub reviews_overdue: u32,
    pub arrears_minor: i64,
    pub held_for_proof_of_life: bool,
}

/// A claim for someone outside the module, with what the caller needs to
/// decide who may see it. Existence of a claim implies health, so the party
/// it is about goes with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRow {
    pub policy: String,
    /// The person the claim is about: the claimant, else the life, else the policyholder.
    pub party: String,
    pub view: ClaimantView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Claim(Option<Box<ClaimRow>>),
    Queue(Vec<QueueRow>),
    Book(Box<Book>),
    Report(Box<Report>),
    Remittance(BTreeMap<String, (u32, i64)>),
    Work(Option<Box<WorkView>>),
    PolicyStatus(Vec<StatusView>),
    Cases(Vec<crate::investigate::CaseView>),
    RepaymentLetters(Vec<LetterDue>),
}

impl ClaimsState {
    /// Answer a question as at `today`, the partition's business date.
    pub fn answer(&self, question: &Question, today: NaiveDate) -> Reply {
        match question {
            Question::Claim { claim } => Reply::Claim(self.claims.get(claim).and_then(|c| {
                Some(Box::new(ClaimRow {
                    policy: c.policy.clone(),
                    party: c.subject_party().to_string(),
                    view: self.claimant_view(claim)?,
                }))
            })),
            Question::Queue { queue, staff } => Reply::Queue(
                self.queue(*queue, today)
                    .into_iter()
                    .filter(|r| self.may_see(staff.as_deref(), &r.claim))
                    .collect(),
            ),
            Question::Book => Reply::Book(Box::new(self.book(today))),
            Question::Report => Reply::Report(Box::new(self.report())),
            Question::Remittance { run } => Reply::Remittance(self.remittance(run)),
            Question::Work { claim, staff } => Reply::Work(self.work_view(claim).filter(|_| self.may_see(staff.as_deref(), claim)).map(Box::new)),
            Question::PolicyStatus { policy, staff } => Reply::PolicyStatus(
                self.policy_status(policy)
                    .into_iter()
                    .filter(|row| match (staff, self.claims.get(&row.claim)) {
                        (Some(staff), Some(claim)) => !self.conflicted(staff, claim),
                        _ => true,
                    })
                    .collect(),
            ),
            Question::Cases { staff } => Reply::Cases(self.cases(staff)),
            Question::RepaymentLetters => Reply::RepaymentLetters(self.repayment_letters()),
        }
    }

    /// Whether someone may see a claim in a queue or its working view: not
    /// if they are conflicted on it. Nobody named is the system asking.
    fn may_see(&self, staff: Option<&str>, claim: &str) -> bool {
        match (staff, self.claims.get(claim)) {
            (Some(s), Some(c)) => !self.conflicted(s, c),
            _ => true,
        }
    }

    pub fn work_view(&self, claim: &str) -> Option<WorkView> {
        let c = self.claims.get(claim)?;
        let task = |t: &crate::model::Task| WorkTask {
            id: t.id.clone(),
            kind: t.kind,
            due: t.due,
            completed_on: t.completed_on,
            overdue: t.overdue,
        };
        let evidence = |e: &crate::model::Evidence| (e.id.clone(), e.kind.clone(), e.from.clone(), e.due, e.received_on);
        Some(WorkView {
            claim: c.id.clone(),
            policy: c.policy.clone(),
            cover: c.cover.clone(),
            life: c.life.clone(),
            claim_type: c.claim_type,
            status: c.status,
            queue: c.queue,
            owner: c.owner.clone(),
            communication_support: c.flags.communication_support,
            event_date: c.event_date,
            notified_on: c.notified_on,
            closed_on: c.closed_on,
            reserve_minor: c.reserve_minor,
            admitted_minor: c.admitted_minor,
            payable_minor: c.payable_minor,
            paid_minor: c.paid_minor(false),
            in_run: c.in_run.clone(),
            tasks: c.tasks.iter().map(task).collect(),
            evidence: c.evidence.iter().map(evidence).collect(),
            waiting: c.pending.as_ref().map(|p| WorkDecision {
                kind: p.request.kind(),
                proposer: p.proposer.clone(),
                proposed_on: p.proposed_on,
                signed: p.signed.clone(),
                still_needed: p.needs.len(),
            }),
            payee: c.payee.as_ref().map(|p| (p.party.clone(), p.kind)),
            interests: c.interests.iter().map(|i| (i.party.clone(), i.owed_minor - i.paid_minor)).collect(),
            payments: c
                .payments
                .iter()
                .map(|p| (p.payment_ref.clone(), p.sent_minor(), p.paid_on, p.returned))
                .collect(),
            benefit: c.schedule.as_ref().map(|s| WorkBenefit {
                monthly_minor: s.monthly_on(s.next_period),
                next_period: s.next_period,
                end: s.end,
                reviews: s.reviews.clone(),
                reviews_overdue: s.reviews_overdue,
                arrears_minor: s.arrears_minor,
                held_for_proof_of_life: c.held_for_proof_of_life(),
            }),
            overpayments: c.overpayments.iter().map(|o| (o.id.clone(), o.outstanding_minor(), o.status)).collect(),
            timers: c.timers.iter().map(|(k, t)| (*k, (t.due, t.met_on, t.escalated))).collect(),
            waiver_from: c.waiver_from,
            continued_from: c.continued_from.clone(),
            scheme_claims: c
                .continued_from
                .as_ref()
                .map(|from| {
                    self.by_policy
                        .get(&from.policy)
                        .into_iter()
                        .flatten()
                        .filter_map(|id| self.claims.get(id))
                        .filter(|s| s.life.as_deref() == Some(from.life.as_str()))
                        .map(|s| (s.id.clone(), s.claim_type, s.status, s.event_date))
                        .collect()
                })
                .unwrap_or_default(),
            definitions: c.cover_check.as_ref().map(|k| k.facts.definitions.clone()).unwrap_or_default(),
            cover_now_and_then: c.cover_check.as_ref().and_then(|k| {
                let f = &k.facts;
                f.known_then
                    .as_ref()
                    .map(|t| {
                        (
                            (f.in_force, f.life_on_policy, f.sum_insured_minor),
                            (t.in_force, t.life_on_policy, t.sum_insured_minor),
                        )
                    })
                    .filter(|(now, then)| now != then)
            }),
            decision_definitions: c
                .decisions
                .iter()
                .filter(|d| !d.definitions.is_empty())
                .map(|d| (d.no, d.definitions.clone()))
                .collect(),
        })
    }

    pub fn status_view(&self, claim: &str) -> Option<StatusView> {
        self.claims.get(claim).map(|c| StatusView {
            claim: c.id.clone(),
            status: c.status,
            notified_on: c.notified_on,
            closed_on: c.closed_on,
        })
    }

    /// Every claim on a policy, oldest first, as [`StatusView`]s.
    pub fn policy_status(&self, policy: &str) -> Vec<StatusView> {
        let mut out: Vec<StatusView> = self.by_policy.get(policy).into_iter().flatten().filter_map(|id| self.status_view(id)).collect();
        out.sort_by(|a, b| (a.notified_on, &a.claim).cmp(&(b.notified_on, &b.claim)));
        out
    }

    pub fn claimant_view(&self, claim: &str) -> Option<ClaimantView> {
        let c = self.claims.get(claim)?;
        let asked = |o: &&crate::Overpayment| matches!(o.status, RecoveryStatus::Pursued | RecoveryStatus::Arrangement | RecoveryStatus::Offset);
        Some(ClaimantView {
            claim: c.id.clone(),
            claim_type: c.claim_type,
            status: c.status,
            owner: c.owner.clone(),
            notified_on: c.notified_on,
            decision_due: c.timers.get(&TimerKind::Decision).filter(|t| t.met_on.is_none()).map(|t| t.due),
            evidence_outstanding: c
                .evidence
                .iter()
                .filter(|e| e.received_on.is_none())
                .map(|e| (e.id.clone(), e.kind.clone(), e.due))
                .collect(),
            admitted_minor: c.admitted_minor,
            paid_minor: c.paid_minor(false) + c.paid_minor(true),
            next_benefit: c
                .schedule
                .as_ref()
                .filter(|s| c.status == Status::InPayment && s.next_period < s.end)
                .map(|s| s.next_period),
            repayment_asked_minor: c.overpayments.iter().filter(asked).map(|o| o.outstanding_minor()).sum(),
            benefit_lines: c
                .schedule
                .iter()
                .flat_map(|s| s.periods.iter())
                .map(|(start, p)| BenefitLine {
                    start: *start,
                    end: p.end,
                    benefit_minor: p.gross_minor - p.arrears_minor + p.other_income_minor,
                    other_income_minor: p.other_income_minor,
                    arrears_minor: p.arrears_minor,
                    overpayment_offset_minor: p.offset_minor,
                    paid_minor: p.gross_minor - p.offset_minor,
                    returned: p.returned,
                })
                .collect(),
        })
    }

    /// A queue's open claims, most urgent first and then oldest first, so
    /// the oldest claim is always visible.
    pub fn queue(&self, queue: Queue, today: NaiveDate) -> Vec<QueueRow> {
        let mut rows: Vec<QueueRow> = self
            .claims
            .values()
            .filter(|c| c.queue == queue && c.status.is_open())
            .map(|c| QueueRow {
                claim: c.id.clone(),
                priority: c.priority + (today - c.event_date).num_days().clamp(0, 30) as u32,
                age_days: (today - c.notified_on).num_days(),
                owner: c.owner.clone(),
            })
            .collect();
        rows.sort_by(|a, b| b.priority.cmp(&a.priority).then(b.age_days.cmp(&a.age_days)));
        rows
    }

    pub fn book(&self, today: NaiveDate) -> Book {
        let mut book = Book::default();
        for c in self.in_payment.iter().filter_map(|id| self.claims.get(id)) {
            let Some(s) = c.schedule.as_ref() else { continue };
            book.in_payment += 1;
            book.monthly_minor += s.monthly_on(today);
            book.reserve_minor += c.reserve_minor;
            let since = c.admission().map_or(c.notified_on, |d| d.decided_on);
            let band = match (today - since).num_days() {
                d if d < 183 => "0-6",
                d if d < 365 => "6-12",
                d if d < 730 => "12-24",
                _ => "24+",
            };
            *book.by_duration.entry(band.to_string()).or_default() += 1;
            let tags = c.cause_code.as_ref().and_then(|code| self.rules.cause_codes.get(code));
            let root = tags.and_then(|t| t.first()).and_then(|t| t.split('.').next()).unwrap_or("unknown");
            *book.by_cause.entry(root.to_string()).or_default() += 1;
            *book.expected_end_by_year.entry(chrono::Datelike::year(&s.expected_end)).or_default() += 1;
            book.reviews_open += c.tasks.iter().filter(|t| t.kind.review() && t.completed_on.is_none()).count() as u32;
            book.reviews_overdue += s.reviews_overdue;
            for t in c.task(TaskKind::ReturnToWork) {
                *book.return_to_work.entry(t.outcome.clone().unwrap_or_else(|| "open".into())).or_default() += 1;
            }
            if let Some(m) = &s.case_manager {
                *book.caseload.entry(m.clone()).or_default() += 1;
            }
        }
        book
    }

    pub fn report(&self) -> Report {
        let mut r = Report::default();
        for c in self.claims.values() {
            *r.notified.entry(c.claim_type).or_default() += 1;
            r.overturns += c.overturns;
            for d in &c.decisions {
                *r.decisions.entry(d.kind).or_default() += 1;
                if let Some(who) = d.signers.first() {
                    *r.by_assessor.entry(who.clone()).or_default().entry(d.kind).or_default() += 1;
                }
                match d.kind {
                    DecisionKind::Admit if d.straight_through => {
                        let category = c.health.as_ref().map_or_else(|| c.claim_type.code().to_string(), |h| h.category.clone());
                        *r.straight_through.entry(category).or_default() += 1;
                    }
                    DecisionKind::Admit if d.amount_minor < c.claimed_minor && c.schedule.is_none() => r.partial_admissions += 1,
                    DecisionKind::ReverseAdmission => r.admissions_reversed += 1,
                    _ => {}
                }
            }
            if let Some(d) = c.decisions.iter().find(|d| matches!(d.kind, DecisionKind::Admit | DecisionKind::Decline)) {
                r.days_to_decision += (d.decided_on - c.notified_on).num_days();
                r.decisions_timed += 1;
            }
            for (kind, t) in &c.timers {
                let entry = r.timers.entry(*kind).or_default();
                match t.met_on {
                    Some(on) if on <= t.due => entry.0 += 1,
                    Some(_) => entry.1 += 1,
                    None => entry.1 += u32::from(t.escalated > 0),
                }
            }
            for o in &c.overpayments {
                r.overpaid_minor += o.amount_minor;
                r.recovered_minor += o.recovered_minor;
                r.written_off_minor += o.written_off_minor;
                if o.status == RecoveryStatus::Held {
                    r.held_minor += o.outstanding_minor();
                }
            }
            r.evidence_cost_minor += c.evidence.iter().map(|e| e.cost_minor).sum::<i64>();
            r.interest_declined_minor += c.payments.iter().map(|p| p.interest_declined_minor).sum::<i64>();
            r.interest_withheld_minor += c.payments.iter().map(|p| p.interest_withheld_minor).sum::<i64>();
            r.benefit_tax_withheld_minor += c
                .schedule
                .iter()
                .flat_map(|s| s.periods.values())
                .filter(|p| !p.returned)
                .map(|p| p.tax_withheld_minor)
                .sum::<i64>();
        }
        r.workload = self
            .owned
            .iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (k.clone(), v.len() as u32))
            .collect();
        r.runs_unreconciled = self.runs.values().filter(|run| run.status == RunStatus::Unreconciled).count() as u32;
        r
    }

    /// Open reserves, unpaid admissions and overpayments outstanding, by
    /// policy. The daily control: these equal the ledger's balances.
    pub fn totals_by_policy(&self) -> BTreeMap<String, Totals> {
        let mut totals: BTreeMap<String, Totals> = BTreeMap::new();
        for c in self.claims.values() {
            let receivable: i64 = c.overpayments.iter().map(|o| o.outstanding_minor()).sum();
            if c.reserve_minor != 0 || c.payable_minor != 0 || receivable != 0 {
                let t = totals.entry(c.policy.clone()).or_default();
                t.reserve_minor += c.reserve_minor;
                t.payable_minor += c.payable_minor;
                t.receivable_minor += receivable;
            }
        }
        totals
    }

    /// The remittance advice for a run: what each payee is paid, and for
    /// how many claims.
    pub fn remittance(&self, run: &str) -> BTreeMap<String, (u32, i64)> {
        let mut advice: BTreeMap<String, (u32, i64)> = BTreeMap::new();
        let failed = |r: &crate::Run, item: &crate::RunItem| r.exceptions.iter().any(|(p, _)| p == &item.payment_ref(&r.id));
        let Some(r) = self.runs.get(run) else { return advice };
        for item in r.items.iter().filter(|i| !failed(r, i)) {
            let entry = advice.entry(item.payee.clone()).or_default();
            entry.0 += 1;
            entry.1 += item.net_minor();
        }
        advice
    }
}

impl ClaimsState {
    fn repayment_letters(&self) -> Vec<LetterDue> {
        let mut due: Vec<LetterDue> = self
            .claims
            .values()
            .flat_map(|c| {
                c.overpayments.iter().filter(|o| o.asking()).flat_map(move |o| {
                    o.letters.iter().filter(|l| !l.requested).map(move |l| LetterDue {
                        request: l.request.clone(),
                        claim: c.id.clone(),
                        policy: c.policy.clone(),
                        party: crate::overpay::payer(c),
                        life: c.life.clone(),
                        overpayment: o.id.clone(),
                        amount_minor: l.amount_minor,
                        asked_on: l.asked_on,
                    })
                })
            })
            .collect();
        due.sort_by(|a, b| (a.asked_on, &a.request).cmp(&(b.asked_on, &b.request)));
        due
    }
}
