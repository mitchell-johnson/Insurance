//! A scheme on the real core with the module beside it, and a stand-in for
//! the module's driver that carries commands, answers and events between
//! the two exactly as the driver does.
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};

use chrono::NaiveDate;
use ic_core::command::LifeSpec;
use ic_core::{Command, GroupId, LifeFactors, LifeId, LifeKind, PartyId, PolicyId, Reject};
use ic_group::command::{caller, Envelope, GroupCommand, Resolution};
use ic_group::event::{GroupEvent, Outbound, ReinsuranceNotice, UnderwritingRequest};
use ic_group::mapping::{Columns, FileKind, FileMapping, SalaryPeriod, SalaryUnit};
use ic_group::model::Member;
use ic_group::record::{MemberAttrs, MemberRow, RowChange};
use ic_group::state::{GroupState, Scheme};
use ic_group::terms::{BillingGroup, ContinuationTerms, CoverEnd, Eligibility, ExcessLoading, ExcessPath, MemberBasis, NoAgreement, SchemeTerms};
use ic_ledger::PartitionId;
use ic_money::{Currency::NZD, Money};
use ic_product::{load_product, ProductDefinition};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command};
use ic_sim::Cluster;

pub const SCHEME: &str = "harbour";

pub fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

pub fn life(id: &str) -> LifeId {
    LifeId(id.to_string())
}

pub fn party(id: &str) -> PartyId {
    PartyId(id.to_string())
}

pub fn product() -> ProductDefinition {
    load_product("group_life.star", include_str!("../../../../products/group_life.star")).unwrap()
}

/// The terms of the fictional employer Harbour Freight's scheme.
pub fn terms() -> SchemeTerms {
    SchemeTerms {
        basis: MemberBasis::Named,
        scheme_year_start: d(2027, 11, 1),
        renewal_date: d(2028, 11, 1),
        rate_guarantee_until: None,
        backdating_limit_days: 90,
        safety_share_percent: 20,
        cover_end: CoverEnd::LeavingDate,
        eligibility: Eligibility {
            waiting_days: 0,
            min_age: Some(16),
            max_age: Some(70),
            min_hours_hundredths: None,
            actively_at_work: false,
        },
        continuation: ContinuationTerms {
            take_up_days: 60,
            products: vec!["term-life".to_string()],
            max_age: Some(65),
        },
        leave_of_absence_days: 90,
        takeover: None,
        billing_groups: vec![
            BillingGroup {
                code: "akl".to_string(),
                name: "Auckland".to_string(),
            },
            BillingGroup {
                code: "wlg".to_string(),
                name: "Wellington".to_string(),
            },
        ],
        excess_path: ExcessPath::UnderwrittenExcess,
        sites: vec![("akl-hq".to_string(), 120)],
        no_agreement: NoAgreement::ContinueOnOldRates { days: 30 },
        renewal_lead_days: 90,
        declaration_due_days: 30,
        file_max_age_days: 14,
        second_approver: false,
        excess_loading: ExcessLoading::EmployerTotal,
        excess_loading_min_count: 0,
        suspend_joiners_after_short_payments: Some(2),
    }
}

pub fn mapping(version: u32) -> FileMapping {
    FileMapping {
        id: "harbour-freight".to_string(),
        version,
        kind: FileKind::Full,
        delimiter: ',',
        columns: Columns {
            employee_id: "Emp No".to_string(),
            ..Columns::default()
        },
        date_format: "%d/%m/%Y".to_string(),
        salary_unit: SalaryUnit::Major,
        salary_period: SalaryPeriod::Annual,
        categories: BTreeMap::new(),
        change_values: BTreeMap::new(),
        content_hash: String::new(),
    }
    .seal()
    .unwrap()
}

pub fn attrs(category: &str, salary_minor: i64, group: &str) -> MemberAttrs {
    MemberAttrs {
        date_of_birth: Some(d(1980, 5, 1)),
        sex: None,
        smoker: None,
        category: Some(category.to_string()),
        salary_minor: Some(salary_minor),
        hours_hundredths: Some(4000),
        employment_start: Some(d(2020, 2, 1)),
        actively_at_work: Some(true),
        billing_group: Some(group.to_string()),
    }
}

/// A row of a full file: who is a member, with their details.
pub fn row(n: u32, employee_id: &str, attrs: MemberAttrs) -> MemberRow {
    MemberRow {
        row: n,
        employee_id: employee_id.to_string(),
        change: None,
        effective: None,
        attrs,
        previous_employee_id: None,
        fingerprint: Some(format!("fp-{employee_id}")),
    }
}

/// A row of a change file.
pub fn change(n: u32, employee_id: &str, what: RowChange, effective: NaiveDate, attrs: MemberAttrs) -> MemberRow {
    MemberRow {
        change: Some(what),
        effective: Some(effective),
        ..row(n, employee_id, attrs)
    }
}

pub struct Harness {
    pub c: Cluster,
    pub g: GroupState,
    pub p: PartitionId,
    pub policy: PolicyId,
    pub group: GroupId,
    pub published: Vec<GroupEvent>,
    pub notices: Vec<ReinsuranceNotice>,
    pub requests: Vec<UnderwritingRequest>,
    /// The requests withdrawn from underwriting, by the key each went under.
    pub withdrawals: Vec<String>,
    /// What underwriting was told the core made of accepted terms.
    pub outcomes: Vec<(String, ic_group::event::IssueOutcome)>,
    pub to_resolve: Vec<(String, Vec<String>)>,
    /// Answers given in place of the core's, one per command sent to it.
    pub core_faults: VecDeque<Option<String>>,
    /// Every command sent to the core, with its key.
    pub sent: Vec<(String, Command)>,
    n: u64,
}

impl Harness {
    /// A scheme from 1 November 2027 with one founding member, E001, on
    /// 60,000 in Auckland, registered with the module.
    pub fn new(terms: SchemeTerms) -> Self {
        let mut c = Cluster::in_memory(1, 11);
        c.open(d(2027, 11, 1)).unwrap();
        for cmd in shipped_activations().unwrap() {
            c.activate_everywhere(cmd).unwrap();
        }
        let founder = attrs("staff", 60_000_00, "akl");
        let pooled = terms.basis == MemberBasis::Pool;
        let first = if pooled {
            LifeSpec {
                id: life("pool:seed"),
                kind: LifeKind::Pool {
                    headcount: 10,
                    total_salary_minor: 600_000_00,
                },
                factors: LifeFactors {
                    category: Some("staff".to_string()),
                    pool: Some(ic_core::PoolAggregate {
                        headcount: 10,
                        total_salary_minor: 600_000_00,
                    }),
                    ..LifeFactors::default()
                },
            }
        } else {
            LifeSpec {
                id: life("E001"),
                kind: LifeKind::Named { party: party("party-1") },
                factors: founder.factors(),
            }
        };
        let issue = group_scheme(30, d(2027, 11, 1), vec![first]);
        let (p, policy, group) = (c.partition_of(&issue.group), issue.policy.clone(), issue.group.clone());
        c.submit(p, "new-business", issue_command(issue)).unwrap();
        c.take_events();
        let mut h = Harness {
            c,
            g: GroupState::default(),
            p,
            policy: policy.clone(),
            group: group.clone(),
            published: vec![],
            notices: vec![],
            requests: vec![],
            withdrawals: Vec::new(),
            outcomes: Vec::new(),
            to_resolve: vec![],
            core_faults: VecDeque::new(),
            sent: vec![],
            n: 0,
        };
        let founding = if pooled {
            vec![]
        } else {
            vec![Member {
                employee_id: "E001".to_string(),
                life: life("E001"),
                attrs: founder,
                joined: d(2027, 11, 1),
                left: None,
                absence_since: None,
                party: Some(party("party-1")),
                fingerprint: Some("fp-E001".to_string()),
                auto_cap_minor: None,
            }]
        };
        h.staff(GroupCommand::RegisterScheme {
            scheme: SCHEME.to_string(),
            policy,
            group,
            employer: party("employer-1"),
            inception: d(2027, 11, 1),
            product: Box::new(product()),
            terms: Box::new(terms),
            founding,
            pools: if pooled { vec!["seed".to_string()] } else { vec![] },
        })
        .unwrap();
        h.staff(GroupCommand::SetMapping {
            scheme: SCHEME.to_string(),
            mapping: Box::new(mapping(1)),
        })
        .unwrap();
        h.settle();
        h
    }

    pub fn today(&self) -> NaiveDate {
        self.c.business_date().unwrap()
    }

    pub fn scheme(&self) -> &Scheme {
        &self.g.schemes[SCHEME]
    }

    /// Send a command to the module without carrying its effects anywhere.
    pub fn send_as(&mut self, caller: &str, actor: &str, command: GroupCommand) -> Result<(), Reject> {
        self.n += 1;
        let env = Envelope {
            key: format!("test-{}", self.n),
            caller: caller.to_string(),
            actor: actor.to_string(),
            today: self.today(),
            command,
        };
        self.g.apply_envelope(env).result
    }

    pub fn staff(&mut self, command: GroupCommand) -> Result<(), Reject> {
        let r = self.send_as(caller::STAFF, "staff-ana", command);
        self.settle();
        r
    }

    pub fn employer(&mut self, command: GroupCommand) -> Result<(), Reject> {
        let r = self.send_as(caller::EMPLOYER, "admin-bo", command);
        self.settle();
        r
    }

    pub fn driver(&mut self, command: GroupCommand) -> Result<(), Reject> {
        self.send_as(caller::DRIVER, "driver", command)
    }

    /// One pass of the driver: deliver everything in the outbox, report the
    /// core's answers, and feed the core's events back. Returns whether
    /// anything was done.
    pub fn step(&mut self) -> bool {
        let pending: Vec<(u64, Outbound)> = self.g.outbox.iter().map(|(id, o)| (*id, o.clone())).collect();
        let mut acked = vec![];
        let mut did = false;
        for (id, out) in pending {
            did = true;
            match out {
                Outbound::Core { key, group, command } => {
                    self.sent.push((key.clone(), (*command).clone()));
                    // A fault stands in for the core's answer; otherwise the core is asked.
                    let error = match self.core_faults.pop_front().flatten() {
                        Some(fault) => Some(fault),
                        None => {
                            let partition = self.c.partition_of(&group);
                            self.c.submit_keyed(partition, &key, "group-admin", *command).result.err().map(|e| e.code)
                        }
                    };
                    self.driver(GroupCommand::CoreResult { key, error }).unwrap();
                }
                Outbound::Publish { event, .. } => {
                    self.published.push(event);
                    acked.push(id);
                }
                Outbound::Reinsurance { notice, .. } => {
                    self.notices.push(notice);
                    acked.push(id);
                }
                Outbound::Underwriting { request, .. } => {
                    self.requests.push(request);
                    acked.push(id);
                }
                Outbound::UnderwritingOutcome { request_key, outcome, .. } => {
                    self.outcomes.push((request_key, outcome));
                    acked.push(id);
                }
                Outbound::WithdrawUnderwriting { request_key, .. } => {
                    self.withdrawals.push(request_key);
                    acked.push(id);
                }
                Outbound::ResolveParties { file, employee_ids, .. } => {
                    self.to_resolve.push((file, employee_ids));
                    acked.push(id);
                }
            }
        }
        if !acked.is_empty() {
            self.driver(GroupCommand::Ack { ids: acked }).unwrap();
        }
        for record in self.c.take_events() {
            for env in ic_group::react(&record) {
                did = true;
                assert!(self.g.apply_envelope(env).result.is_ok());
            }
        }
        did
    }

    pub fn settle(&mut self) {
        for _ in 0..10_000 {
            if !self.step() {
                return;
            }
        }
        panic!("the module and the core never settled");
    }

    /// Close this many business days, with the module's daily tick each morning.
    pub fn days(&mut self, n: u32) {
        for _ in 0..n {
            let close = self.c.end_of_day().unwrap();
            assert!(close.breaks.is_empty(), "reconciliation breaks on {}: {:?}", close.closed, close.breaks);
            self.settle();
        }
    }

    pub fn pay(&mut self, minor: i64) {
        self.n += 1;
        let command = Command::ApplyReceipt {
            policy: self.policy.clone(),
            receipt: format!("receipt-{}", self.n),
            amount: Money::new(minor, NZD),
        };
        self.c.submit(self.p, "cash-matching", command).unwrap();
        self.settle();
    }

    pub fn owed(&self) -> i64 {
        self.c.state(self.p).policies[&self.policy].outstanding_minor()
    }

    /// Receive, scan and submit the rows of a file, leaving it compared.
    pub fn upload(&mut self, file: &str, kind: FileKind, rows: Vec<MemberRow>) -> Result<(), Reject> {
        self.employer(GroupCommand::ReceiveFile {
            scheme: SCHEME.to_string(),
            file: file.to_string(),
            content_hash: format!("hash-{file}"),
        })?;
        self.driver(GroupCommand::RecordScan {
            scheme: SCHEME.to_string(),
            file: file.to_string(),
            clean: true,
        })?;
        // In parts, as the driver sends a large file.
        let parts: Vec<Vec<MemberRow>> = if rows.is_empty() {
            vec![vec![]]
        } else {
            rows.chunks(2000).map(<[MemberRow]>::to_vec).collect()
        };
        let count = parts.len();
        let mut r = Ok(());
        for (n, part) in parts.into_iter().enumerate() {
            r = self.driver(GroupCommand::SubmitRows {
                scheme: SCHEME.to_string(),
                file: file.to_string(),
                mapping_hash: mapping(1).content_hash,
                kind,
                file_date: self.today(),
                rows: part,
                rejected: vec![],
                last: n + 1 == count,
            });
            if r.is_err() {
                break;
            }
        }
        self.settle();
        r
    }

    pub fn approve(&mut self, file: &str) -> Result<(), Reject> {
        self.employer(GroupCommand::ApproveFile {
            scheme: SCHEME.to_string(),
            file: file.to_string(),
        })
    }

    /// Answer the customer master's part for a file: every joiner is a new
    /// person unless `answer` says otherwise.
    pub fn resolve(&mut self, file: &str, answer: impl Fn(&str) -> Option<Resolution>) {
        let ids: Vec<String> = self.to_resolve.iter().filter(|(f, _)| f == file).flat_map(|(_, ids)| ids.clone()).collect();
        self.to_resolve.retain(|(f, _)| f != file);
        if ids.is_empty() {
            // Nobody to ask about: the file went straight to the core.
            return;
        }
        let resolutions = ids
            .into_iter()
            .map(|id| {
                let r = answer(&id).unwrap_or_else(|| Resolution::Created {
                    party: party(&format!("party-{id}")),
                });
                (id, r)
            })
            .collect();
        self.driver(GroupCommand::PartiesResolved {
            scheme: SCHEME.to_string(),
            file: file.to_string(),
            resolutions,
            last: true,
        })
        .unwrap();
        self.settle();
    }

    /// Upload, approve and apply a file whose joiners are all new people.
    pub fn apply(&mut self, file: &str, kind: FileKind, rows: Vec<MemberRow>) {
        self.upload(file, kind, rows).unwrap();
        self.approve(file).unwrap();
        self.resolve(file, |_| None);
    }

    /// As `apply`, on a scheme that needs a second person: one administrator
    /// uploads and another approves.
    pub fn apply_as_two(&mut self, file: &str, kind: FileKind, rows: Vec<MemberRow>) {
        self.upload(file, kind, rows).unwrap();
        self.send_as(
            caller::EMPLOYER,
            "admin-cy",
            GroupCommand::ApproveFile {
                scheme: SCHEME.to_string(),
                file: file.to_string(),
            },
        )
        .unwrap();
        self.settle();
        self.resolve(file, |_| None);
    }

    /// Carry the one command waiting for the core as a driver might after a
    /// delay: the core's events reach the module before the core's answer.
    pub fn core_events_before_answer(&mut self) {
        let (key, group, command) = self
            .g
            .outbox
            .values()
            .find_map(|o| match o {
                Outbound::Core { key, group, command } => Some((key.clone(), group.clone(), (**command).clone())),
                _ => None,
            })
            .expect("a command for the core");
        let partition = self.c.partition_of(&group);
        let error = self.c.submit_keyed(partition, &key, "group-admin", command).result.err().map(|e| e.code);
        for record in self.c.take_events() {
            for env in ic_group::react(&record) {
                assert!(self.g.apply_envelope(env).result.is_ok());
            }
        }
        self.driver(GroupCommand::CoreResult { key, error }).unwrap();
    }

    /// The core's underwritten layers on a member: amount and end date.
    pub fn excess_layers(&self, id: &str) -> Vec<(i64, Option<NaiveDate>)> {
        self.c.state(self.p).policies[&self.policy]
            .covers
            .values()
            .flat_map(|c| c.layers.iter())
            .filter(|l| l.life == life(id) && l.origin == ic_core::LayerOrigin::UnderwrittenExcess)
            .map(|l| (l.sum_insured_minor, l.end))
            .collect()
    }

    /// A change file of one salary change, applied.
    pub fn salary(&mut self, file: &str, id: &str, category: &str, salary_minor: i64) {
        let today = self.today();
        self.apply(
            file,
            FileKind::Changes,
            vec![change(1, id, RowChange::Change, today, attrs(category, salary_minor, "wlg"))],
        );
    }

    pub fn underwriting(&mut self, case: u64, decision: ic_group::command::ExcessDecision) -> Result<(), Reject> {
        let r = self.send_as(
            caller::UNDERWRITING,
            "underwriter-di",
            GroupCommand::RecordExcessDecision {
                scheme: SCHEME.to_string(),
                case,
                decision,
            },
        );
        self.settle();
        r
    }

    pub fn core_lives(&self) -> Vec<String> {
        self.c.state(self.p).policies[&self.policy]
            .lives
            .values()
            .filter(|l| l.left.is_none())
            .map(|l| l.id.0.clone())
            .collect()
    }

    pub fn saw(&self, f: impl Fn(&GroupEvent) -> bool) -> bool {
        self.published.iter().any(f)
    }
}
