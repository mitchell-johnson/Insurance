//! A small claims shop: rules, an authority book, a handful of staff and a
//! stand-in core holding three policies.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_claims::command::{Notification, Periodic};
use ic_claims::config::{Exclusion, HealthBenefit, Line, Profile, Queue, Role, Staff};
use ic_claims::testing::FakeCore;
use ic_claims::{
    Actor, AuthorityBook, Channel, ClaimType, ClaimsState, Command, CoverFacts, DecisionKind, Flags, Notifier, PayeeKind, Reject, Request, Rules, TaskKind,
};

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).expect("a real date")
}

pub fn actor(name: &str) -> Actor {
    Actor {
        staff: name.to_string(),
        purpose: "assessing the claim".to_string(),
    }
}

fn set<T: Ord + Clone>(items: &[T]) -> BTreeSet<T> {
    items.iter().cloned().collect()
}

pub fn rules() -> Rules {
    let all = [
        DecisionKind::Reserve,
        DecisionKind::Admit,
        DecisionKind::Withdraw,
        DecisionKind::ExGratia,
        DecisionKind::EndBenefit,
        DecisionKind::RecoveryStatus,
        DecisionKind::WriteOff,
    ];
    Rules {
        version: 1,
        escalation_days: [1, 3, 7],
        straight_through_max_minor: [(ClaimType::Health, 1_000), (ClaimType::Trauma, 5_000)].into(),
        large_claim_minor: 1_000_000,
        two_signature_admission_minor: 300_000,
        two_signature_ex_gratia_minor: 800,
        two_signature_write_off_minor: 400,
        initial_reserve_pct: [(ClaimType::IncomeProtection, 1200)].into(),
        sharp_reserve_move_pct: 50,
        reason_codes: [
            ("R-OK".to_string(), set(&all)),
            ("R-NO".to_string(), set(&[DecisionKind::Decline])),
            ("R-REV".to_string(), set(&[DecisionKind::ReverseAdmission])),
            ("R-OVR".to_string(), set(&[DecisionKind::Admit])),
        ]
        .into(),
        cause_codes: [
            ("CC-0001".to_string(), vec!["musculoskeletal.back".to_string()]),
            ("CC-0002".to_string(), vec!["mental.depression".to_string()]),
            ("CC-0003".to_string(), vec!["cancer.skin".to_string()]),
        ]
        .into(),
        exclusions: [
            (
                "EX-0001".to_string(),
                Exclusion {
                    applies_to: BTreeSet::new(),
                    cause_tags: vec!["musculoskeletal".to_string()],
                    verbatim: false,
                },
            ),
            (
                "EX-9001".to_string(),
                Exclusion {
                    verbatim: true,
                    ..Exclusion::default()
                },
            ),
        ]
        .into(),
        medical_review_months: 6,
        income_declaration_months: 3,
        review_grace_days: 14,
        max_offset_pct: 25,
        late_death_days: 30,
        estate_simplified_max_minor: 100_000,
        health_benefits: [(
            "dental".to_string(),
            HealthBenefit {
                per_event_minor: 1_000,
                per_year_minor: 2_000,
                lifetime_minor: 5_000,
                copay_pct: 20,
                excess_minor: 50,
            },
        )]
        .into(),
        pre_approval_days: 30,
        recurrence_days: 180,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
        ..Rules::default()
    }
}

fn staff(profile: &str, roles: &[Role], queues: &[Queue]) -> Staff {
    Staff {
        profile: profile.to_string(),
        roles: set(roles),
        queues: set(queues),
        wip_limit: 0,
        vulnerable_trained: false,
        related_parties: BTreeSet::new(),
    }
}

pub fn authority() -> AuthorityBook {
    let lines = set(&[Line::Health, Line::LumpSum, Line::IncomeProtection]);
    let lump = |max: i64| -> BTreeMap<ClaimType, i64> {
        [
            (ClaimType::Death, max),
            (ClaimType::TerminalIllness, max),
            (ClaimType::Trauma, max),
            (ClaimType::TotalPermanentDisability, max),
            (ClaimType::IncomeProtection, max),
            (ClaimType::PremiumWaiver, max),
            (ClaimType::Health, max),
        ]
        .into()
    };
    let assessor = Profile {
        lines: lines.clone(),
        max_admission_minor: lump(600_000),
        max_payment_minor: 600_000,
        max_reserve_minor: 700_000,
        may_decline: true,
        max_ex_gratia_minor: 1_000,
        max_write_off_minor: 500,
        ..Profile::default()
    };
    let senior = Profile {
        lines: lines.clone(),
        max_admission_minor: lump(5_000_000),
        max_payment_minor: 5_000_000,
        max_reserve_minor: 9_000_000,
        may_decline: true,
        max_ex_gratia_minor: 50_000,
        max_write_off_minor: 500_000,
        may_override_exclusion: true,
        may_reverse_admission: true,
        may_change_recovery_status: true,
        senior: true,
    };
    let junior = Profile {
        lines: set(&[Line::Health]),
        max_admission_minor: [(ClaimType::Health, 500)].into(),
        max_payment_minor: 500,
        max_reserve_minor: 500,
        ..Profile::default()
    };
    let reviewer = Profile { lines, ..Profile::default() };
    let work = [
        Queue::Health,
        Queue::LumpSum,
        Queue::IncomeProtectionNew,
        Queue::IncomeProtectionOngoing,
        Queue::Group,
    ];
    let senior_work = [
        Queue::Health,
        Queue::LumpSum,
        Queue::IncomeProtectionNew,
        Queue::IncomeProtectionOngoing,
        Queue::Group,
        Queue::ComplexLarge,
        Queue::Recoveries,
    ];
    let mut people: BTreeMap<String, Staff> = BTreeMap::new();
    people.insert("ana".into(), staff("assessor", &[Role::Assessor], &work));
    people.insert("ben".into(), staff("assessor", &[Role::Assessor], &work));
    people.insert("una".into(), staff("assessor", &[Role::Assessor], &work));
    people.insert("rel".into(), staff("assessor", &[Role::Assessor], &work));
    people.insert("jon".into(), staff("junior", &[Role::Assessor], &[Queue::Health]));
    people.insert("sam".into(), staff("senior", &[Role::Assessor, Role::QueueLead], &senior_work));
    people.insert("sue".into(), staff("senior", &[Role::Assessor, Role::ClaimsManager], &senior_work));
    people.insert("mo".into(), staff("reviewer", &[Role::MedicalOfficer], &[Queue::MedicalOfficer]));
    people.insert("quin".into(), staff("reviewer", &[Role::QualityReviewer], &[Queue::QualityReview]));
    people.insert("cody".into(), staff("reviewer", &[Role::ConductOfficer], &[]));
    people.insert("lex".into(), staff("reviewer", &[Role::Legal], &[]));
    people.insert("rec".into(), staff("reviewer", &[Role::Assessor], &[Queue::Recoveries]));
    people.insert("ivy".into(), staff("reviewer", &[Role::Investigator], &[]));
    if let Some(s) = people.get_mut("ben") {
        s.vulnerable_trained = true;
    }
    if let Some(s) = people.get_mut("rel") {
        s.related_parties = set(&["party-holder".to_string()]);
    }
    AuthorityBook {
        version: 1,
        profiles: [
            ("assessor".to_string(), assessor),
            ("senior".to_string(), senior),
            ("junior".to_string(), junior),
            ("reviewer".to_string(), reviewer),
        ]
        .into(),
        staff: people,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
    }
}

/// Cover as the policy record has it: in force, paid up, 500,000 insured.
pub fn facts(cover_type: &str) -> CoverFacts {
    CoverFacts {
        policy_status: "in_force".into(),
        in_force: true,
        standing: Some("paid".into()),
        premium_paid_since_event_minor: 0,
        outstanding_premium_minor: 0,
        cover_type: cover_type.into(),
        cover_start: Some(d(2025, 1, 1)),
        sum_insured_minor: 500_000,
        exclusions: vec![],
        definitions: vec!["trauma-def@3".into()],
        life_on_policy: true,
        life_party: Some("party-life".into()),
        policy_year_start: Some(d(2027, 1, 1)),
        ends_on_death: true,
        policyholder: "party-holder".into(),
        product_code: "LIFE".into(),
        product_version: "hash-1".into(),
        interests: vec![],
        nominations: vec![],
        continuation_of: None,
        waiting_days: 0,
        policy_loan_minor: 0,
        joint_lives: vec![],
        reduced_by: vec![],
        chosen_excess_minor: None,
    }
}

pub struct World {
    pub state: ClaimsState,
    pub core: FakeCore,
    pub index: u64,
}

pub const TODAY: (i32, u32, u32) = (2027, 3, 1);

impl World {
    /// Rules and authority active, three policies on the partition.
    pub fn new() -> Self {
        let mut core = FakeCore::new(d(TODAY.0, TODAY.1, TODAY.2));
        core.cover("pol-1", "life", facts("life"));
        core.cover("pol-1", "trauma", facts("trauma"));
        core.cover(
            "pol-2",
            "ip",
            CoverFacts {
                sum_insured_minor: 4_000,
                ..facts("income_protection")
            },
        );
        core.cover(
            "pol-3",
            "health",
            CoverFacts {
                sum_insured_minor: 0,
                ..facts("health")
            },
        );
        let mut w = World {
            state: ClaimsState::default(),
            core,
            index: 0,
        };
        w.ok(Command::ActivateRules { rules: Box::new(rules()) });
        w.ok(Command::ActivateAuthority { book: Box::new(authority()) });
        w
    }

    pub fn run(&mut self, command: Command) -> Result<(), Reject> {
        self.index += 1;
        self.state.apply(&mut self.core, self.index, command)
    }

    /// Apply a command that must succeed.
    pub fn ok(&mut self, command: Command) {
        let name = command.name();
        if let Err(e) = self.run(command) {
            panic!("{name} was refused: {e}");
        }
    }

    /// Apply a command that must be refused, and return the code. A refused
    /// command must leave nothing behind: no state, no instruction, no notice.
    pub fn refused(&mut self, command: Command) -> String {
        let before = (self.state.clone(), self.core.instructions.len(), self.core.notices.len());
        let name = command.name();
        let Err(e) = self.run(command) else {
            panic!("{name} was accepted");
        };
        assert_eq!(self.state, before.0, "{name} was refused with {} but changed state", e.code);
        assert_eq!(self.core.instructions.len(), before.1, "{name} was refused but instructed the core");
        assert_eq!(self.core.notices.len(), before.2, "{name} was refused but published a notice");
        e.code
    }

    pub fn advance(&mut self, days: u64) {
        self.core.today = self.core.today.checked_add_days(chrono::Days::new(days)).expect("a real date");
    }

    pub fn on(&mut self, date: NaiveDate) {
        self.core.today = date;
    }

    pub fn claim(&self, id: &str) -> &ic_claims::Claim {
        self.state.claims.get(id).expect("the claim exists")
    }

    pub fn notification(id: &str, policy: &str, cover: &str, claim_type: ClaimType) -> Notification {
        Notification {
            claim: id.into(),
            policy: policy.into(),
            cover: cover.into(),
            life: Some("life-1".into()),
            claim_type,
            event_date: d(2027, 2, 20),
            channel: Channel::App,
            notifier: Notifier::Policyholder,
            claimant: Some("party-claimant".into()),
            claimed_minor: None,
            flags: Flags::default(),
            health: None,
            underwriters: vec!["una".into()],
            catastrophe_event: None,
            cause_code: None,
        }
    }

    /// A trauma claim on pol-1, notified, acknowledged and owned by ana.
    pub fn open_claim(&mut self, id: &str) {
        self.ok(Command::Notify(Box::new(Self::notification(id, "pol-1", "trauma", ClaimType::Trauma))));
        self.ok(Command::Acknowledge {
            claim: id.into(),
            delivery_ref: "delivered-1".into(),
        });
        self.assign(id, "ana");
    }

    pub fn assign(&mut self, id: &str, to: &str) {
        self.ok(Command::Assign {
            claim: id.into(),
            to: to.into(),
            actor: actor("sam"),
        });
    }

    /// Record a cause and the cover check, as the owner.
    pub fn assess(&mut self, id: &str, cause: &str) {
        let owner = self.claim(id).owner.clone().expect("the claim has an owner");
        self.ok(Command::RecordCause {
            claim: id.into(),
            cause_code: cause.into(),
            actor: actor(&owner),
        });
        self.ok(Command::CoverCheck {
            claim: id.into(),
            relied_on: vec!["in_force".into(), "sum_insured".into()],
            actor: actor(&owner),
        });
    }

    pub fn decide_command(id: &str, who: &str, request: Request, reason: &str) -> Command {
        Command::Decide {
            claim: id.into(),
            request,
            reasons: vec![reason.into()],
            actor: actor(who),
        }
    }

    pub fn decide(&mut self, id: &str, who: &str, request: Request, reason: &str) -> Result<(), Reject> {
        self.run(Self::decide_command(id, who, request, reason))
    }

    /// A decision that must be accepted, at once or as a proposal.
    pub fn decided(&mut self, id: &str, who: &str, request: Request, reason: &str) {
        self.ok(Self::decide_command(id, who, request, reason));
    }

    /// A decision that must be refused, leaving nothing behind.
    pub fn undecided(&mut self, id: &str, who: &str, request: Request, reason: &str) -> String {
        self.refused(Self::decide_command(id, who, request, reason))
    }

    pub fn approved(&mut self, id: &str, who: &str) {
        self.ok(Command::Approve {
            claim: id.into(),
            actor: actor(who),
        });
    }

    pub fn unapproved(&mut self, id: &str, who: &str) -> String {
        self.refused(Command::Approve {
            claim: id.into(),
            actor: actor(who),
        })
    }

    /// Hold the conversation with the claimant, as the owner.
    pub fn conversation(&mut self, id: &str, task: &str) {
        let owner = self.claim(id).owner.clone().expect("the claim has an owner");
        self.task(id, task, TaskKind::ClaimantConversation, &owner);
        self.complete(id, task, "held", &owner).expect("the owner completes the conversation");
    }

    /// Hold the open case conference on a claim.
    pub fn conference(&mut self, id: &str, who: &str) {
        let task = self
            .claim(id)
            .task(TaskKind::CaseConference)
            .find(|t| t.completed_on.is_none())
            .map(|t| t.id.clone())
            .expect("a case conference is open");
        self.complete(id, &task, "held", who).expect("the conference is held");
    }

    pub fn admit_lump(amount: Option<i64>) -> Request {
        Request::Admit {
            amount_minor: amount,
            periodic: None,
            exclusion_override: None,
            exclusion_reviewed: false,
            payer_deceased: false,
        }
    }

    pub fn task(&mut self, id: &str, task: &str, kind: TaskKind, who: &str) {
        self.ok(Command::OpenTask {
            claim: id.into(),
            task: task.into(),
            kind,
            due: None,
            actor: actor(who),
        });
    }

    pub fn complete(&mut self, id: &str, task: &str, outcome: &str, who: &str) -> Result<(), Reject> {
        self.run(Command::CompleteTask {
            claim: id.into(),
            task: task.into(),
            outcome: outcome.into(),
            attendees: vec!["sam".into(), "sue".into()],
            vault_ref: Some("vault-1".into()),
            spend_minor: 0,
            actor: actor(who),
        })
    }

    pub fn verify_payee(&mut self, id: &str, who: &str) {
        self.ok(Command::VerifyPayee {
            claim: id.into(),
            party: "party-claimant".into(),
            kind: PayeeKind::Policyholder,
            account_ref: "acct-1".into(),
            name_check_ref: "name-1".into(),
            screening_ref: "screen-1".into(),
            authority_doc: None,
            actor: actor(who),
        });
    }

    /// An income protection claim on pol-2, admitted by ana at 3,000 a
    /// month from 1 March 2027 to 1 March 2029, with a verified payee.
    pub fn benefit_in_payment(&mut self, id: &str) {
        self.ok(Command::Notify(Box::new(Self::notification(id, "pol-2", "ip", ClaimType::IncomeProtection))));
        self.ok(Command::Acknowledge {
            claim: id.into(),
            delivery_ref: "delivered-1".into(),
        });
        self.assign(id, "ana");
        self.assess(id, "CC-0002");
        let request = Request::Admit {
            amount_minor: None,
            periodic: Some(Periodic {
                monthly_minor: 3_000,
                first_period: d(2027, 3, 1),
                end: d(2029, 3, 1),
                expected_end: d(2028, 3, 1),
            }),
            exclusion_override: None,
            exclusion_reviewed: false,
            payer_deceased: false,
        };
        self.decide(id, "ana", request, "R-OK").expect("the admission is within ana's authority");
        self.verify_payee(id, "ana");
    }

    /// Prepare and approve a benefit run for a date.
    pub fn pay_run(&mut self, run: &str, benefit_date: NaiveDate) {
        self.ok(Command::PrepareRun {
            run: run.into(),
            kind: ic_claims::RunKind::Benefit,
            benefit_date,
            actor: actor("ana"),
        });
        self.ok(Command::ApproveRun {
            run: run.into(),
            actor: actor("sam"),
        });
    }
}
