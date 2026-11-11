//! The synthetic insurer: a simulated year, by seed.
//!
//! This is the spine's exit test as a library, so the same run backs the CI
//! test, the `ic-simulate` binary and any larger soak. Policies are issued,
//! billed, paid, lapsed, reinstated, cancelled and transferred; a group
//! scheme takes a monthly membership file with backdated changes; every
//! reconciliation must be clean every day; and at the end each partition's
//! recorded log must replay to an identical state hash.

use std::collections::BTreeMap;

use chrono::{Datelike, Days, NaiveDate};
use ic_cashmatch::{IncomingPayment, Matcher, Outcome};
use ic_core::command::MemberChange;
use ic_core::event::Event;
use ic_core::{Command, CoreState, GroupId, LifeFactors, LifeId, PolicyId, PolicyStatus, APPLY_BUDGET};
use ic_ledger::{resum, verify_chain, Hash32};
use ic_log::Replica;
use ic_money::Money;
use ic_readmodel::Projections;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serde::Serialize;

use crate::config::shipped_activations;
use crate::persona::{group_scheme, issue_command, member, random_persona, PayBehaviour, Persona};
use crate::{Cluster, Delivery};

#[derive(Debug, Clone)]
pub struct YearConfig {
    pub seed: u64,
    pub partitions: u16,
    pub retail_policies: u64,
    pub scheme_members: u32,
    pub days: u32,
    pub start: NaiveDate,
    /// A policy transfer between groups is started every this many days.
    pub transfer_every_days: u32,
    /// Keep journals in memory to verify hash chains and re-sum postings.
    pub verify_journal: bool,
}

impl YearConfig {
    pub fn small(seed: u64) -> Self {
        Self {
            seed,
            partitions: 4,
            retail_policies: 240,
            scheme_members: 900,
            days: 366,
            // 15 November 2027; the year includes 29 February 2028 and both clock changes.
            start: NaiveDate::from_ymd_opt(2027, 11, 15).unwrap_or(NaiveDate::MIN),
            transfer_every_days: 20,
            verify_journal: true,
        }
    }

    /// The `ic-simulate` command line over the small insurer:
    /// `[--seed N] [--policies N] [--members N] [--days N] [--partitions N] [--no-journal]`.
    pub fn from_args(args: &[String]) -> Result<Self, String> {
        let mut cfg = Self::small(20271115);
        let mut args = args.iter();
        while let Some(flag) = args.next() {
            if flag == "--no-journal" {
                cfg.verify_journal = false;
                continue;
            }
            let mut number = |max: u64| -> Result<u64, String> {
                let value = args.next().ok_or_else(|| format!("{flag} needs a number"))?;
                let n: u64 = value.parse().map_err(|_| format!("{flag}: {value:?} is not a number"))?;
                if n > max {
                    return Err(format!("{flag} must be at most {max}"));
                }
                Ok(n)
            };
            match flag.as_str() {
                "--seed" => cfg.seed = number(u64::MAX)?,
                "--policies" => cfg.retail_policies = number(u64::MAX)?,
                "--members" => cfg.scheme_members = number(u64::from(u32::MAX))? as u32,
                "--days" => cfg.days = number(u64::from(u32::MAX))? as u32,
                "--partitions" => {
                    cfg.partitions = number(u64::from(ic_ledger::PARTITION_COUNT))? as u16;
                    if cfg.partitions == 0 {
                        return Err("--partitions must be at least 1".to_string());
                    }
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(cfg)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct YearReport {
    pub days: u32,
    pub policies_issued: u64,
    pub commands: u64,
    pub journal_entries: u64,
    pub receipts: u64,
    pub dishonours: u64,
    pub lapses: u64,
    pub reinstatements: u64,
    pub cancellations: u64,
    pub anniversaries: u64,
    pub transfers_completed: u64,
    pub membership_changes: u64,
    pub cash_exceptions: u64,
    pub policy_exceptions: u64,
    pub max_work_units: u32,
    pub final_merkle_root: String,
    pub state_hashes: BTreeMap<u16, String>,
}

struct Payer {
    persona: Persona,
    payments: u32,
    /// (receipt, reverse on, pay again on)
    bounce: Option<(String, NaiveDate, NaiveDate)>,
    bounced: bool,
    paid_ahead: bool,
    done: bool,
}

fn fail<T>(what: impl Into<String>) -> Result<T, String> {
    Err(what.into())
}

/// Run the synthetic insurer. Returns the cluster as it ended and a report,
/// or the first thing that went wrong.
pub fn run_year(cfg: &YearConfig) -> Result<(Cluster, YearReport), String> {
    let mut rng = StdRng::seed_from_u64(cfg.seed);
    let mut c = Cluster::in_memory(cfg.partitions, cfg.seed);
    c.keep_journal = cfg.verify_journal;
    c.open(cfg.start).map_err(|e| e.to_string())?;
    for cmd in shipped_activations().map_err(|e| e.to_string())? {
        c.activate_everywhere(cmd).map_err(|e| e.to_string())?;
    }
    let mut view = Projections::new();
    let matcher = Matcher::default();
    let mut report = YearReport::default();
    let mut payers: Vec<Payer> = Vec::new();
    let mut receipt_n = 0u64;
    let mut transfer_n = 0u64;
    let mut last_root = Hash32::ZERO;

    // The employer: one scheme, loaded in chunks like a membership file.
    let scheme_n = 9_000_000u64;
    let mut members: Vec<LifeId> = Vec::new();
    let mut next_member = 0u32;
    let new_member = |rng: &mut StdRng, next: &mut u32| {
        let m = member(
            &format!("m{:06}", *next),
            NaiveDate::from_ymd_opt(rng.gen_range(1962..=2004), rng.gen_range(1..=12), rng.gen_range(1..=28)).unwrap_or(NaiveDate::MIN),
            if rng.gen_bool(0.1) { "executive" } else { "staff" },
            rng.gen_range(55..=240) * 1_000_00,
        );
        *next += 1;
        m
    };
    let (scheme_id, scheme_group) = (PolicyId(format!("pol-{scheme_n:08}")), GroupId(format!("grp-{scheme_n:08}")));
    let scheme_partition = c.partition_of(&scheme_group);
    if cfg.scheme_members > 0 {
        let first: Vec<_> = (0..cfg.scheme_members.min(100)).map(|_| new_member(&mut rng, &mut next_member)).collect();
        members.extend(first.iter().map(|m| m.id.clone()));
        c.submit(scheme_partition, "new-business", issue_command(group_scheme(scheme_n, cfg.start, first)))
            .map_err(|e| e.to_string())?;
        report.policies_issued += 1;
        while next_member < cfg.scheme_members {
            let changes: Vec<_> = (0..(cfg.scheme_members - next_member).min(100))
                .map(|_| MemberChange::Joined {
                    life: new_member(&mut rng, &mut next_member),
                    effective: cfg.start,
                })
                .collect();
            members.extend(changes.iter().filter_map(|c| match c {
                MemberChange::Joined { life, .. } => Some(life.id.clone()),
                _ => None,
            }));
            report.membership_changes += changes.len() as u64;
            c.submit(
                scheme_partition,
                "group-admin",
                Command::ApplyMembership {
                    policy: scheme_id.clone(),
                    changes,
                },
            )
            .map_err(|e| e.to_string())?;
        }
    }

    let issue_window = 60.min(cfg.days.max(1));
    for day in 0..cfg.days {
        let today = c.business_date().ok_or("no business date")?;

        // --- new business, spread over the first weeks ---
        let due_today = if day < issue_window {
            cfg.retail_policies * (day as u64 + 1) / issue_window as u64 - cfg.retail_policies * day as u64 / issue_window as u64
        } else {
            0
        };
        for _ in 0..due_today {
            let n = payers.len() as u64 + 1;
            let persona = random_persona(&mut rng, n, today);
            let p = c.partition_of(&persona.group);
            c.submit(p, "new-business", issue_command(persona.issue.clone()))
                .map_err(|e| format!("issue {}: {e}", persona.policy))?;
            report.policies_issued += 1;
            payers.push(Payer {
                persona,
                payments: 0,
                bounce: None,
                bounced: false,
                paid_ahead: false,
                done: false,
            });
        }

        // --- the stream feeds the read models ---
        absorb(&mut c, &mut view, &mut report)?;

        // --- money arrives, is matched, and is applied ---
        for payer in payers.iter_mut().filter(|p| !p.done) {
            let id = &payer.persona.policy;
            let Some(entry) = view.policy(id) else { continue };
            let (status, partition) = (entry.status, entry.partition);
            let outstanding = view.billing(id).map_or(0, |b| b.outstanding_minor);
            let oldest_due = view.billing(id).and_then(|b| b.oldest_unpaid_due);
            let mut pay: Option<i64> = None;

            if let Some((receipt, reverse_on, repay_on)) = payer.bounce.clone() {
                if today == reverse_on {
                    // The bank returns the direct debit.
                    if c.submit(partition, "cash-matching", Command::ReverseReceipt { policy: id.clone(), receipt })
                        .is_ok()
                    {
                        report.dishonours += 1;
                    }
                    continue;
                }
                if today < repay_on {
                    continue;
                }
                payer.bounce = None;
            }

            match payer.persona.behaviour {
                PayBehaviour::CancelsAfter(after) if status == PolicyStatus::InForce && today >= payer.persona.issue.inception + Days::new(after as u64) => {
                    match c.submit(partition, "policy-servicing", Command::CancelPolicy { policy: id.clone() }) {
                        Ok(()) => {
                            report.cancellations += 1;
                            payer.done = true;
                            // The refund is paid out through the payments module.
                            let held = c
                                .state(partition)
                                .ledger
                                .available(&ic_core::accounts::policy_suspense(id, payer.persona.issue_currency()).id);
                            if held > 0 {
                                c.submit(
                                    partition,
                                    "policy-servicing",
                                    Command::RefundSuspense {
                                        policy: id.clone(),
                                        amount: Money::new(held as i64, payer.persona.issue_currency()),
                                        with_claim: None,
                                    },
                                )
                                .map_err(|e| e.to_string())?;
                            }
                        }
                        // In flight between groups: try again tomorrow.
                        Err(e) if e.code == "policy_transferring" => {}
                        Err(e) => return fail(format!("cancel {id}: {e}")),
                    }
                    continue;
                }
                PayBehaviour::StopsAfter(k) if payer.payments >= k => {}
                PayBehaviour::LapsesThenReinstates(k) if payer.payments >= k && !payer.paid_ahead => {
                    if status == PolicyStatus::Lapsed {
                        if let Some(lapse) = c.state(partition).policies.get(id).and_then(|p| p.lapse.clone()) {
                            if today >= lapse.lapsed_on + Days::new(10) {
                                // Pay the arrears and a few periods more, then ask to come back.
                                receipt_n += 1;
                                let amount = lapse.arrears_minor * 3 + 500_00;
                                c.submit_keyed(
                                    partition,
                                    &format!("rcpt-{receipt_n}"),
                                    "cash-matching",
                                    Command::ApplyReceipt {
                                        policy: id.clone(),
                                        receipt: format!("rcpt-{receipt_n}"),
                                        amount: Money::new(amount, payer.persona.issue_currency()),
                                    },
                                )
                                .result
                                .map_err(|e| format!("arrears {id}: {e}"))?;
                                report.receipts += 1;
                                match c.submit(partition, "collections", Command::Reinstate { policy: id.clone() }) {
                                    Ok(()) => {
                                        report.reinstatements += 1;
                                        payer.paid_ahead = true;
                                    }
                                    Err(e) if e.code == "policy_transferring" => {}
                                    Err(e) => return fail(format!("reinstate {id}: {e}")),
                                }
                            }
                        }
                    }
                }
                PayBehaviour::Late(n) => {
                    if outstanding > 0 && oldest_due.is_some_and(|d| today >= d + Days::new(n as u64)) {
                        pay = Some(outstanding);
                    }
                }
                PayBehaviour::PaysAhead if !payer.paid_ahead && outstanding > 0 => {
                    payer.paid_ahead = true;
                    pay = Some(outstanding * 12);
                }
                _ => {
                    if outstanding > 0 && status == PolicyStatus::InForce {
                        pay = Some(outstanding);
                    }
                }
            }

            if let Some(amount) = pay {
                receipt_n += 1;
                // One payment in fifty arrives with no usable reference.
                let reference = (!rng.gen_ratio(1, 50)).then(|| format!("Premium {}", payer.persona.number));
                let payment = IncomingPayment {
                    id: format!("rcpt-{receipt_n}"),
                    amount: Money::new(amount, payer.persona.issue_currency()),
                    reference,
                    payer_account: None,
                    received_on: today,
                };
                let command = match matcher.match_payment(&view, &payment) {
                    Outcome::Apply { command, .. } => command,
                    Outcome::Exception { .. } => {
                        // The collections team works the exception item and allocates it by hand.
                        report.cash_exceptions += 1;
                        Command::ApplyReceipt {
                            policy: id.clone(),
                            receipt: payment.id.clone(),
                            amount: payment.amount,
                        }
                    }
                };
                match c.submit_keyed(partition, &payment.id, "cash-matching", command).result {
                    Ok(()) => {
                        report.receipts += 1;
                        payer.payments += 1;
                        if payer.persona.behaviour == PayBehaviour::OneDishonour && payer.payments == 3 && !payer.bounced {
                            payer.bounced = true;
                            payer.bounce = Some((payment.id, today + Days::new(2), today + Days::new(9)));
                        }
                    }
                    Err(e) if e.code == "policy_transferring" => {}
                    Err(e) => return fail(format!("receipt for {id}: {e}")),
                }
            }
        }

        // --- the employer pays its invoice ten days after it falls due ---
        if cfg.scheme_members > 0 {
            if let Some(b) = view
                .billing(&scheme_id)
                .filter(|b| b.outstanding_minor > 0 && b.oldest_unpaid_due.is_some_and(|d| today >= d + Days::new(10)))
            {
                receipt_n += 1;
                let cmd = Command::ApplyReceipt {
                    policy: scheme_id.clone(),
                    receipt: format!("rcpt-{receipt_n}"),
                    amount: Money::new(b.outstanding_minor, ic_money::Currency::NZD),
                };
                c.submit_keyed(scheme_partition, &format!("rcpt-{receipt_n}"), "cash-matching", cmd)
                    .result
                    .map_err(|e| format!("scheme receipt: {e}"))?;
                report.receipts += 1;
            }
            // --- the monthly membership file, with late notifications ---
            if today.day() == 20 && day > 0 {
                let mut changes = Vec::new();
                for _ in 0..(members.len() / 100).max(1) {
                    let back = rng.gen_range(5..=40);
                    let effective = (today - Days::new(back)).max(cfg.start);
                    let i = rng.gen_range(0..members.len());
                    changes.push(MemberChange::Left {
                        life: members.swap_remove(i),
                        effective,
                    });
                    let joiner = new_member(&mut rng, &mut next_member);
                    members.push(joiner.id.clone());
                    changes.push(MemberChange::Joined {
                        life: joiner,
                        effective: (today - Days::new(rng.gen_range(0..=20))).max(cfg.start),
                    });
                    let who = members[rng.gen_range(0..members.len() - 1)].clone();
                    let effective = (today - Days::new(rng.gen_range(0..=35))).max(cfg.start);
                    if let Some(f) = c
                        .state(scheme_partition)
                        .policies
                        .get(&scheme_id)
                        .and_then(|p| p.lives.get(&who))
                        .filter(|l| l.in_force_on(effective))
                        .and_then(|l| l.factors.as_of(effective))
                    {
                        changes.push(MemberChange::Changed {
                            life: who,
                            effective,
                            factors: LifeFactors {
                                salary_minor: f.salary_minor.map(|s| s + 5_000_00),
                                ..f.clone()
                            },
                        });
                    }
                }
                for chunk in changes.chunks(60) {
                    report.membership_changes += chunk.len() as u64;
                    c.submit(
                        scheme_partition,
                        "group-admin",
                        Command::ApplyMembership {
                            policy: scheme_id.clone(),
                            changes: chunk.to_vec(),
                        },
                    )
                    .map_err(|e| format!("membership file on {today}: {e}"))?;
                }
            }
        }

        // --- now and then a policy changes owner and moves between groups ---
        if cfg.transfer_every_days > 0 && day > 0 && day % cfg.transfer_every_days == 0 && !payers.is_empty() {
            let pick = &payers[rng.gen_range(0..payers.len())].persona.policy;
            if let Some(entry) = view.policy(pick).filter(|e| matches!(e.status, PolicyStatus::InForce | PolicyStatus::Lapsed)) {
                transfer_n += 1;
                let target_group = GroupId(format!("grp-moved-{transfer_n:05}"));
                let target_partition = c.partition_of(&target_group);
                let _ = c.submit(
                    entry.partition,
                    "transfer",
                    Command::BeginTransfer {
                        policy: pick.clone(),
                        transfer_id: format!("xfer-{transfer_n:05}"),
                        target_group,
                        target_partition,
                    },
                );
            }
        }
        // The relay is unreliable on purpose: messages are delayed and duplicated.
        c.relay_messages(|_| match rng.gen_range(0..10) {
            0..=2 => Delivery::Delay,
            3..=4 => Delivery::Duplicate,
            _ => Delivery::Deliver,
        });

        // --- cutoff, end-of-day, reconciliation, Merkle root ---
        let close = c.end_of_day().map_err(|e| format!("end of day {today}: {e}"))?;
        if !close.breaks.is_empty() {
            return fail(format!("reconciliation breaks on {today}: {:?}", close.breaks));
        }
        last_root = close.merkle_root;
        report.days += 1;
    }

    // Let every transfer land, then take stock.
    for _ in 0..100 {
        if c.messages_in_flight() == 0 {
            break;
        }
        c.relay_messages(|_| Delivery::Deliver);
    }
    absorb(&mut c, &mut view, &mut report)?;
    if !c.relay_errors.is_empty() {
        return fail(format!("relay errors: {:?}", c.relay_errors));
    }
    let breaks = c.reconcile();
    if !breaks.is_empty() {
        return fail(format!("final reconciliation breaks: {breaks:?}"));
    }

    for (p, node) in &c.nodes {
        report.commands += node.commands;
        report.journal_entries += node.entries;
        report.max_work_units = report.max_work_units.max(node.max_work_units);
        report.state_hashes.insert(*p, node.state().state_hash().to_hex());
    }
    let policies: Vec<PolicyId> = payers.iter().map(|payer| payer.persona.policy.clone()).collect();
    audit(&c, &view, &policies, cfg.verify_journal)?;
    report.final_merkle_root = last_root.to_hex();
    Ok((c, report))
}

/// The independent checks on a finished run: every partition's log replays
/// to the state it has, its journal verifies as a chain and re-sums to the
/// balances held, no command exceeded the apply budget, the read models
/// followed every event, and the policy directory places each policy where
/// it really is. Any failure names what is wrong.
pub fn audit(c: &Cluster, view: &Projections, policies: &[PolicyId], verify_journal: bool) -> Result<(), String> {
    for (p, node) in &c.nodes {
        // A recorded log replays to an identical state hash.
        let replayed = Replica::recover(node.replica.log.clone(), CoreState::new(*p)).map_err(|e| e.to_string())?;
        if replayed.state.state_hash() != node.state().state_hash() {
            return fail(format!("partition {p}: replay produced a different state hash"));
        }
        if verify_journal {
            // An independent verifier walks the chain; re-summing the postings gives the balances.
            let head = verify_chain(*p, &node.journal).map_err(|e| format!("partition {p}: {e}"))?;
            if head != node.state().ledger.head() {
                return fail(format!("partition {p}: chain head does not match"));
            }
            let sums = resum(&node.journal);
            for (account, balance) in node.state().ledger.accounts() {
                if sums.get(&account.id).copied().unwrap_or(0) != balance {
                    return fail(format!("partition {p}: materialised balance of {} has drifted", account.id));
                }
            }
        }
        // The read models followed every partition without a gap.
        if view.checkpoint(*p) != node.state().last_event_seq() {
            return fail(format!(
                "partition {p}: read model is at {}, stream at {}",
                view.checkpoint(*p),
                node.state().last_event_seq()
            ));
        }
        if node.max_work_units > APPLY_BUDGET {
            return fail(format!("a command used {} work units; the apply budget is {APPLY_BUDGET}", node.max_work_units));
        }
    }
    // The policy directory agrees with where every policy really is.
    for id in policies {
        let actual = c.locate(id);
        let listed = view.policy(id).map(|e| e.partition);
        if actual != listed {
            return fail(format!("directory lists {id} on {listed:?}, it is on {actual:?}"));
        }
        if let (Some(p), Some(e)) = (actual, view.policy(id)) {
            if c.state(p).policies[id].status != e.status {
                return fail(format!("directory has the wrong status for {id}"));
            }
        }
    }
    Ok(())
}

fn absorb(c: &mut Cluster, view: &mut Projections, report: &mut YearReport) -> Result<(), String> {
    for e in c.take_events() {
        match &e.event {
            Event::PolicyStatusChanged { to: PolicyStatus::Lapsed, .. } => report.lapses += 1,
            Event::AnniversaryReached { .. } => report.anniversaries += 1,
            Event::TransferCompleted { .. } => report.transfers_completed += 1,
            Event::PolicyException { policy, code, message } => {
                report.policy_exceptions += 1;
                return Err(format!("end-of-day exception on {policy}: {code}: {message}"));
            }
            _ => {}
        }
        view.apply(&e).map_err(|e| e.to_string())?;
    }
    Ok(())
}
