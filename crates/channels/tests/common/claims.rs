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
use ic_core::{Command, Envelope, PolicyId};
use ic_ledger::PartitionId;

type Asked<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'a>>;

struct Part {
    state: ClaimsState,
    core: FakeCore,
    index: u64,
    seen: BTreeMap<String, Proposed>,
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
        max_admission_minor: [(ClaimType::Trauma, 600_000)].into(),
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

    fn propose(&self, partition: PartitionId, envelope: Envelope) -> Asked<'_, Proposed> {
        let mut parts = self.0.lock().unwrap();
        let part = &mut parts[usize::from(partition)];
        let proposed = if let Some(first) = part.seen.get(&envelope.key) {
            Proposed {
                duplicate: true,
                ..first.clone()
            }
        } else {
            let Command::Claims(command) = envelope.command else {
                panic!("the console sent the core something other than a claims command");
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

    fn propose(&self, _partition: PartitionId, _envelope: Envelope) -> Asked<'_, Proposed> {
        Box::pin(async { Err("no leader".to_string()) })
    }
}
