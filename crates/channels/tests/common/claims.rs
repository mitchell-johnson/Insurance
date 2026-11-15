//! Claims for the console's tests: the claims module's own state machine on
//! two partitions, each behind its stand-in core, answering and applying as
//! a partition does. Rules and an authority book naming three of the
//! fixture's staff are in force on both.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use chrono::NaiveDate;
use ic_channels::claims::{Partitions, Proposed};
use ic_core::claims_module::config::{Line, Profile, Queue, Role, Staff};
use ic_core::claims_module::report::{Question, Reply};
use ic_core::claims_module::testing::FakeCore;
use ic_core::claims_module::{AuthorityBook, ClaimType, ClaimsState, Command as ClaimsCommand, CoverFacts, DecisionKind, Rules};
use ic_core::command::IssuePolicy;
use ic_core::{Command, Envelope, PolicyId};
use ic_ledger::PartitionId;
use serde_json::{json, Value};

type Asked<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'a>>;

struct Part {
    state: ClaimsState,
    core: FakeCore,
    index: u64,
    seen: BTreeMap<String, Proposed>,
    /// Servicing commands the portal carried here, with the caller named.
    servicing: Vec<(String, Command)>,
    /// The refusal the stand-in core gives the next servicing command, if any.
    refuse_servicing: Option<(String, String)>,
}

impl Part {
    fn apply(&mut self, command: ClaimsCommand) -> Proposed {
        self.index += 1;
        let before = self.core.notices.len();
        match self.state.apply(&mut self.core, self.index, command) {
            Ok(()) => Proposed {
                duplicate: false,
                events: self.core.notices.len() - before,
                rejected: None,
            },
            Err(e) => Proposed {
                duplicate: false,
                events: 0,
                rejected: Some((e.code, e.message)),
            },
        }
    }
}

/// Two partitions: `pol-c0` on partition 0, `pol-c1` (life and trauma) on 1.
pub struct ClaimsCore(Mutex<Vec<Part>>);

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn facts(cover_type: &str) -> CoverFacts {
    CoverFacts {
        policy_status: "in_force".into(),
        in_force: true,
        standing: Some("paid".into()),
        cover_type: cover_type.into(),
        cover_start: Some(d(2025, 1, 1)),
        sum_insured_minor: 500_000,
        life_on_policy: true,
        life_party: Some("party-life".into()),
        policy_year_start: Some(d(2027, 1, 1)),
        ends_on_death: true,
        policyholder: "party-holder".into(),
        product_code: "LIFE".into(),
        product_version: "hash-1".into(),
        ..CoverFacts::default()
    }
}

fn rules() -> Rules {
    let all: BTreeSet<DecisionKind> = [DecisionKind::Reserve, DecisionKind::Admit, DecisionKind::Withdraw].into();
    Rules {
        version: 1,
        escalation_days: [1, 3, 7],
        large_claim_minor: 1_000_000,
        two_signature_admission_minor: 300_000,
        reason_codes: [("R-OK".to_string(), all)].into(),
        cause_codes: [("CC-0003".to_string(), vec!["cancer.skin".to_string()])].into(),
        medical_review_months: 6,
        income_declaration_months: 3,
        review_grace_days: 14,
        max_offset_pct: 25,
        late_death_days: 30,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
        ..Rules::default()
    }
}

fn authority() -> AuthorityBook {
    let lines: BTreeSet<Line> = [Line::LumpSum].into();
    let queues: BTreeSet<Queue> = [Queue::LumpSum, Queue::ComplexLarge].into();
    let assessor = Profile {
        lines,
        max_admission_minor: [(ClaimType::Trauma, 600_000), (ClaimType::Death, 600_000)].into(),
        max_payment_minor: 600_000,
        max_reserve_minor: 700_000,
        may_decline: true,
        ..Profile::default()
    };
    let person = |roles: &[Role]| Staff {
        profile: "assessor".into(),
        roles: roles.iter().cloned().collect(),
        queues: queues.clone(),
        wip_limit: 0,
        vulnerable_trained: false,
        related_parties: BTreeSet::new(),
    };
    AuthorityBook {
        version: 1,
        profiles: [("assessor".to_string(), assessor)].into(),
        staff: [
            ("cl-1".to_string(), person(&[Role::Assessor])),
            ("cl-2".to_string(), person(&[Role::Assessor])),
            ("cl-lead".to_string(), person(&[Role::Assessor, Role::QueueLead])),
        ]
        .into(),
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
    }
}

/// Configured on both partitions, business date 1 March 2027.
pub fn seeded() -> ClaimsCore {
    let parts = (0..2)
        .map(|p| {
            let mut core = FakeCore::new(d(2027, 3, 1));
            if p == 0 {
                core.cover("pol-c0", "life", facts("life"));
            } else {
                core.cover("pol-c1", "life", facts("life"));
                core.cover("pol-c1", "trauma", facts("trauma"));
            }
            let mut part = Part {
                state: ClaimsState::default(),
                core,
                index: 0,
                seen: BTreeMap::new(),
                servicing: Vec::new(),
                refuse_servicing: None,
            };
            for command in [
                ClaimsCommand::ActivateRules { rules: Box::new(rules()) },
                ClaimsCommand::ActivateAuthority { book: Box::new(authority()) },
            ] {
                assert_eq!(part.apply(command).rejected, None);
            }
            part
        })
        .collect();
    ClaimsCore(Mutex::new(parts))
}

impl ClaimsCore {
    /// Put one more policy's life cover on a partition's stand-in core.
    pub fn cover(&self, partition: usize, policy: &str) {
        self.0.lock().unwrap()[partition].core.cover(policy, "life", facts("life"));
    }

    /// The servicing commands one partition's stand-in core took, with their callers.
    pub fn servicing(&self, partition: usize) -> Vec<(String, Command)> {
        self.0.lock().unwrap()[partition].servicing.clone()
    }

    /// The next servicing command on a partition is refused with this code.
    pub fn refuse_servicing(&self, partition: usize, code: &str) {
        self.0.lock().unwrap()[partition].refuse_servicing = Some((code.to_string(), format!("the core refused it: {code}")));
    }

    /// What one partition's stand-in core has been asked to do.
    pub fn instructions(&self, partition: usize) -> usize {
        self.0.lock().unwrap()[partition].core.instructions.len()
    }
}

impl Partitions for ClaimsCore {
    fn count(&self) -> PartitionId {
        2
    }

    fn ask(&self, partition: PartitionId, question: Question) -> Asked<'_, Option<Reply>> {
        let parts = self.0.lock().unwrap();
        let part = &parts[usize::from(partition)];
        let reply = part.state.answer(&question, part.core.today);
        Box::pin(async move { Ok(Some(reply)) })
    }

    fn locate(&self, partition: PartitionId, policy: PolicyId) -> Asked<'_, Option<PartitionId>> {
        let held = self.0.lock().unwrap()[usize::from(partition)]
            .core
            .covers
            .keys()
            .any(|k| k.starts_with(&format!("{}/", policy.0)));
        Box::pin(async move { Ok(held.then_some(partition)) })
    }

    /// Two pages on the first partition and one on the second, so paging is walked.
    fn loadings(&self, partition: PartitionId, after: Option<PolicyId>, as_at: chrono::NaiveDate) -> Asked<'_, ic_core::loadings::LoadingsPage> {
        use ic_core::loadings::{LoadingRow, LoadingsPage};
        let row = |policy: &str, loading: &str| LoadingRow {
            policy: PolicyId::from(policy),
            cover: "life".into(),
            layer: 1,
            start: as_at,
            end: None,
            loadings: vec![serde_json::from_value(serde_json::json!(loading)).unwrap()],
        };
        let page = match (partition, after.as_ref().map(|p| p.0.as_str())) {
            (0, None) => LoadingsPage {
                rows: vec![row("pol-a", "0.5")],
                next: Some(PolicyId::from("pol-a")),
            },
            // No leader for this date once the first page has been read.
            (0, Some("pol-a")) if as_at.to_string() == "1999-01-01" => return Box::pin(async { Err("no leader".to_string()) }),
            (0, Some("pol-a")) => LoadingsPage {
                rows: vec![row("pol-b", "0.25")],
                next: None,
            },
            _ => LoadingsPage {
                rows: vec![row("pol-z", "1")],
                next: None,
            },
        };
        Box::pin(async move { Ok(page) })
    }

    /// A bill of 12.34 for any product but `none`, which the core refuses,
    /// and `later`, which waits for a business date to open.
    fn quote(&self, partition: PartitionId, application: IssuePolicy) -> Asked<'_, Option<Result<Value, (String, String)>>> {
        let answer = match application.product_code.as_str() {
            "later" => None,
            "none" => Some(Err(("unknown_product".to_string(), "there is no product none".to_string()))),
            _ => Some(Ok(json!({ "total_minor": 1234, "product": application.product_code, "partition": partition }))),
        };
        Box::pin(async move { Ok(answer) })
    }

    /// `pol-c0` on partition 0 and `pol-c1` on 1, as the read side has them.
    fn policy(&self, partition: PartitionId, number: String) -> Asked<'_, Option<Value>> {
        let held = (number == format!("pol-c{partition}"))
            .then(|| json!({ "entry": { "partition": partition }, "policy": { "number": number, "policyholder": "kiri" }, "billing": null }));
        Box::pin(async move { Ok(held) })
    }

    /// Partition 0 is clean; partition 1 has a break and money in transit.
    fn closing(&self, partition: PartitionId) -> Asked<'_, Value> {
        let view = match partition {
            0 => {
                json!({ "partition": 0, "business_date": "2027-11-15", "breaks": [], "unfinished": [], "transfer_in_transit": "0", "transfer_in_flight": false })
            }
            _ => {
                json!({ "partition": 1, "business_date": "2027-11-16", "breaks": [{ "check": "suspense_clear", "detail": "12.00 in suspense" }], "unfinished": ["pol-c1"], "transfer_in_transit": "0", "transfer_in_flight": false })
            }
        };
        Box::pin(async move { Ok(view) })
    }

    fn propose(&self, partition: PartitionId, envelope: Envelope) -> Asked<'_, Proposed> {
        let mut parts = self.0.lock().unwrap();
        let part = &mut parts[usize::from(partition)];
        let proposed = if let Some(first) = part.seen.get(&envelope.key) {
            Proposed {
                duplicate: true,
                ..first.clone()
            }
        } else {
            let command = match envelope.command {
                Command::Claims(command) => command,
                other => {
                    // A servicing request carried from the app: checked for its caller as the core would, then kept.
                    let rejected = if !other.allowed_callers().contains(&envelope.caller.as_str()) {
                        Some(("caller".to_string(), format!("{} may not send this", envelope.caller)))
                    } else {
                        part.refuse_servicing.take()
                    };
                    if rejected.is_none() {
                        part.servicing.push((envelope.caller.clone(), other));
                    }
                    let proposed = Proposed {
                        duplicate: false,
                        events: usize::from(rejected.is_none()),
                        rejected,
                    };
                    part.seen.insert(envelope.key.clone(), proposed.clone());
                    return Box::pin(async move { Ok(proposed) });
                }
            };
            let proposed = if command.allowed_callers().contains(&envelope.caller.as_str()) {
                part.apply(*command)
            } else {
                Proposed {
                    duplicate: false,
                    events: 0,
                    rejected: Some(("caller".into(), format!("{} may not send this", envelope.caller))),
                }
            };
            part.seen.insert(envelope.key.clone(), proposed.clone());
            proposed
        };
        Box::pin(async move { Ok(proposed) })
    }
}

/// Partitions with no leader.
pub struct Down;

impl Partitions for Down {
    fn count(&self) -> PartitionId {
        2
    }

    fn ask(&self, _partition: PartitionId, _question: Question) -> Asked<'_, Option<Reply>> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn locate(&self, _partition: PartitionId, _policy: PolicyId) -> Asked<'_, Option<PartitionId>> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn loadings(&self, _partition: PartitionId, _after: Option<PolicyId>, _as_at: chrono::NaiveDate) -> Asked<'_, ic_core::loadings::LoadingsPage> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn quote(&self, _partition: PartitionId, _application: IssuePolicy) -> Asked<'_, Option<Result<Value, (String, String)>>> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn policy(&self, _partition: PartitionId, _number: String) -> Asked<'_, Option<Value>> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn closing(&self, _partition: PartitionId) -> Asked<'_, Value> {
        Box::pin(async { Err("no leader".to_string()) })
    }

    fn propose(&self, _partition: PartitionId, _envelope: Envelope) -> Asked<'_, Proposed> {
        Box::pin(async { Err("no leader".to_string()) })
    }
}
