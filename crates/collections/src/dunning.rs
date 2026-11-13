//! Following the core: what it says about each policy, and the steps of the
//! sequence each closed day brings.
//!
//! A partition closes a business day by advancing its date and then running
//! its end-of-day, which settles standing for the new day. The steps for a
//! day are therefore taken when the partition closes the day after it: by
//! then every payment and dishonour of that day, and the end-of-day that
//! followed, are in.

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId, PolicyStatus, Standing};

use crate::config::{Step, StepKind};
use crate::machine::{CollectionsState, CoreFact, Ctx, Event, Reject};
use crate::model::{Kept, Notice, NoticeState, Pause, Task, TaskKind, Tracked};

/// A premium for a period, scaled to a year.
pub fn annualise(period_start: NaiveDate, period_end: NaiveDate, total_minor: i64) -> i64 {
    let days = ((period_end - period_start).num_days() + 1).max(1);
    let scaled = i128::from(total_minor) * 365 / i128::from(days);
    i64::try_from(scaled).unwrap_or(i64::MAX)
}

impl CollectionsState {
    pub(crate) fn observe(&mut self, partition: u16, on: NaiveDate, fact: CoreFact, ctx: &mut Ctx) {
        match fact {
            CoreFact::Issued {
                policy,
                number,
                party,
                product,
            } => {
                if !self.sequences.contains_key(&product) {
                    self.unsequenced.insert(product.clone());
                }
                self.policies.entry(policy).or_insert(Tracked {
                    number,
                    party,
                    product,
                    partition,
                    annual_premium_minor: 0,
                    standing: Some(Standing::Paid),
                    oldest_unpaid_due: None,
                    pause: None,
                    done: Default::default(),
                    kept: None,
                    hold: None,
                    relief: None,
                    ended: false,
                });
            }
            CoreFact::PremiumDue {
                policy,
                period_start,
                period_end,
                total_minor,
            } => {
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.annual_premium_minor = annualise(period_start, period_end, total_minor);
                }
            }
            CoreFact::Standing { policy, to, oldest_unpaid_due } => self.standing(policy, to, oldest_unpaid_due, ctx),
            CoreFact::Status { policy, to } => self.status(policy, to, ctx),
            CoreFact::ArrangementMade {
                policy,
                arrangement,
                arrears_minor,
                dues,
            } => {
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.kept = Some(Kept {
                        id: arrangement.clone(),
                        made_on: on,
                        arrears_minor,
                        dues,
                    });
                    ctx.emit(Event::ArrangementKept { policy, arrangement });
                }
            }
            CoreFact::ArrangementCompleted { policy, arrangement } => {
                if let Some(p) = self.policies.get_mut(&policy) {
                    p.kept = None;
                    ctx.emit(Event::ArrangementCompleted { policy, arrangement });
                }
            }
            CoreFact::ArrangementBroken {
                policy,
                arrangement,
                outstanding_minor,
            } => self.broken(policy, arrangement, outstanding_minor, on, ctx),
            CoreFact::DayClosed { closed } => self.day_closed(partition, closed, ctx),
        }
    }

    fn standing(&mut self, policy: PolicyId, to: Option<Standing>, oldest: Option<NaiveDate>, ctx: &mut Ctx) {
        let Some(p) = self.policies.get_mut(&policy) else {
            return;
        };
        p.standing = to;
        p.oldest_unpaid_due = oldest;
        let overdue = oldest.is_some() && matches!(to, Some(Standing::InGrace | Standing::Suspended));
        if overdue {
            self.in_arrears.insert(policy);
        } else if self.in_arrears.remove(&policy) {
            // Out of arrears: the next spell starts the sequence afresh. A
            // dispute or a referral was about these arrears and ends with
            // them; a death does not.
            p.done.clear();
            p.hold = p.hold.filter(|h| h.stops_notices());
            ctx.emit(Event::SequenceStopped {
                policy,
                why: "paid".to_string(),
            });
        }
    }

    fn status(&mut self, policy: PolicyId, to: PolicyStatus, ctx: &mut Ctx) {
        let Some(p) = self.policies.get_mut(&policy) else {
            return;
        };
        p.ended = to != PolicyStatus::InForce;
        if p.ended {
            p.kept = None;
            if self.in_arrears.remove(&policy) {
                ctx.emit(Event::SequenceStopped {
                    policy,
                    why: format!("{to:?}").to_lowercase(),
                });
            }
        }
    }

    fn broken(&mut self, policy: PolicyId, arrangement: String, outstanding_minor: i64, on: NaiveDate, ctx: &mut Ctx) {
        let Some(p) = self.policies.get_mut(&policy) else {
            return;
        };
        if let Some(kept) = p.kept.take() {
            // The days the arrangement ran are not counted against the
            // owner, for premium that fell due before it was made.
            let ran = (on - kept.made_on).num_days().max(0);
            let before = p.pause.map_or(0, |q| q.days);
            p.pause = Some(Pause {
                due_before: kept.made_on,
                days: before + ran,
            });
        }
        let party = p.party.clone();
        ctx.emit(Event::ArrangementBroken {
            policy: policy.clone(),
            arrangement,
            outstanding_minor,
        });
        self.open_task(&party, TaskKind::Arrangement, &policy, ctx);
    }

    /// The steps a closed day brings to one partition's policies, and the
    /// call tasks that have waited too long.
    fn day_closed(&mut self, partition: u16, day: NaiveDate, ctx: &mut Ctx) {
        let due: Vec<PolicyId> = self
            .in_arrears
            .iter()
            .filter(|id| self.policies.get(*id).is_some_and(|p| p.partition == partition && !p.ended))
            .cloned()
            .collect();
        for policy in due {
            self.steps_for(&policy, day, ctx);
        }
        self.escalate(partition, day, ctx);
    }

    fn steps_for(&mut self, policy: &PolicyId, day: NaiveDate, ctx: &mut Ctx) {
        let Some(mut p) = self.policies.get(policy).cloned() else {
            return;
        };
        if p.kept.is_some() {
            return;
        }
        if let Some(case) = p.relief.clone() {
            // From the decision to the relief's last day nothing is sent:
            // the customer has been told what will happen.
            if self.hardship.get(&case).and_then(|c| c.last_day()).is_some_and(|last| day <= last) {
                return;
            }
            p.relief = None;
        }
        if let Some(crate::model::Hold::HardshipReferral { until }) = p.hold {
            if day > until {
                p.hold = None;
            }
        }
        let (Some(sequence), Some(age)) = (self.sequences.get(&p.product).cloned(), p.age(day)) else {
            self.policies.insert(policy.clone(), p);
            return;
        };
        let reached: Vec<&Step> = sequence
            .steps
            .iter()
            .filter(|s| i64::from(sequence.day_of(s, p.annual_premium_minor)) <= age && !p.done.contains(&s.day))
            .collect();
        let held = p.hold.is_some();
        let silent = p.hold.is_some_and(|h| h.stops_notices());
        let latest_reminder = reached.iter().rev().find(|s| !s.kind.mandatory()).map(|s| s.day);
        let mut send: Vec<Step> = Vec::new();
        for step in reached {
            p.done.insert(step.day);
            let goes = if step.kind.mandatory() {
                !silent
            } else {
                !held && Some(step.day) == latest_reminder
            };
            if goes {
                send.push(step.clone());
            } else {
                ctx.emit(Event::StepPassed {
                    policy: policy.clone(),
                    day: step.day,
                    kind: step.kind,
                });
            }
        }
        let party = p.party.clone();
        self.policies.insert(policy.clone(), p);
        for step in send {
            ctx.emit(Event::StepReached {
                policy: policy.clone(),
                day: step.day,
                kind: step.kind,
            });
            if step.kind == StepKind::CallTask {
                self.open_task(&party, TaskKind::Call, policy, ctx);
            } else {
                self.raise_notice(&party, &step, day, policy, ctx);
            }
        }
    }

    /// One notice per party, step and day, whatever number of its policies
    /// reached the step: a policy joins the day's notice while it has not
    /// been taken for sending.
    fn raise_notice(&mut self, party: &PartyId, step: &Step, day: NaiveDate, policy: &PolicyId, ctx: &mut Ctx) {
        let base = format!("n:{party}:{}:{day}", step.kind.name());
        let mut id = base.clone();
        let mut n = 1;
        while self.notices.get(&id).is_some_and(|x| x.state != NoticeState::Due) {
            n += 1;
            id = format!("{base}:{n}");
        }
        let notice = self.notices.entry(id.clone()).or_insert_with(|| Notice {
            id: id.clone(),
            party: party.clone(),
            kind: step.kind,
            channel: step.channel,
            template: step.template.clone(),
            on: day,
            policies: Vec::new(),
            proof_of_delivery: step.kind.mandatory(),
            state: NoticeState::Due,
        });
        notice.policies.push(policy.clone());
        ctx.emit(Event::NoticeRaised {
            notice: id,
            party: party.clone(),
            kind: step.kind,
            policies: notice.policies.clone(),
        });
    }

    /// A task for a person. A party has one open task of a kind: a second
    /// policy joins it.
    pub(crate) fn open_task(&mut self, party: &PartyId, kind: TaskKind, policy: &PolicyId, ctx: &mut Ctx) -> String {
        let existing = self
            .open_tasks
            .iter()
            .find(|id| self.tasks.get(*id).is_some_and(|t| t.party == *party && t.kind == kind))
            .cloned();
        let id = existing.unwrap_or_else(|| self.next("task"));
        let task = self.tasks.entry(id.clone()).or_insert_with(|| Task {
            id: id.clone(),
            party: party.clone(),
            kind,
            policies: Vec::new(),
            opened: ctx.today,
            attempts: 0,
            escalated: false,
            closed: None,
        });
        if !task.policies.contains(policy) {
            task.policies.push(policy.clone());
        }
        self.open_tasks.insert(id.clone());
        ctx.emit(Event::TaskOpened {
            task: id.clone(),
            party: party.clone(),
            kind,
            policies: task.policies.clone(),
        });
        id
    }

    /// A call task left open past its product's limit goes to the team lead,
    /// once.
    fn escalate(&mut self, partition: u16, day: NaiveDate, ctx: &mut Ctx) {
        let late: Vec<String> = self
            .open_tasks
            .iter()
            .filter(|id| {
                let Some(task) = self.tasks.get(*id) else { return false };
                let first = task.policies.first().and_then(|p| self.policies.get(p));
                let limit = first.and_then(|p| self.sequences.get(&p.product)).map(|s| s.call_escalate_days);
                task.kind == TaskKind::Call
                    && !task.escalated
                    && first.is_some_and(|p| p.partition == partition)
                    && limit.is_some_and(|days| (day - task.opened).num_days() >= i64::from(days))
            })
            .cloned()
            .collect();
        for id in late {
            if let Some(task) = self.tasks.get_mut(&id) {
                task.escalated = true;
            }
            ctx.emit(Event::TaskEscalated { task: id });
        }
    }

    pub(crate) fn notice_sent(&mut self, id: &str) -> Result<(), Reject> {
        let notice = self.notices.get_mut(id).ok_or_else(|| Reject::new("unknown_notice", "no such notice"))?;
        if notice.state == NoticeState::Due {
            notice.state = NoticeState::Sent;
        }
        Ok(())
    }

    /// A mandatory notice that did not arrive is a task: the customer must
    /// be reached another way before the policy can lapse.
    pub(crate) fn notice_delivery(&mut self, id: &str, delivered: bool, ctx: &mut Ctx) -> Result<(), Reject> {
        let notice = self.notices.get_mut(id).ok_or_else(|| Reject::new("unknown_notice", "no such notice"))?;
        let was = notice.state;
        notice.state = if delivered { NoticeState::Delivered } else { NoticeState::Undeliverable };
        if delivered || was == NoticeState::Undeliverable || !notice.proof_of_delivery {
            return Ok(());
        }
        let (party, policies) = (notice.party.clone(), notice.policies.clone());
        let mut task = String::new();
        for policy in &policies {
            task = self.open_task(&party, TaskKind::UndeliverableNotice, policy, ctx);
        }
        ctx.emit(Event::NoticeUndeliverable { notice: id.to_string(), task });
        Ok(())
    }
}
