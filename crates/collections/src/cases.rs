//! What people do: record what a customer said, agree an arrangement, and
//! apply for, decide and end hardship relief.

use chrono::Duration;
use ic_core::PolicyId;

use crate::machine::{CollectionsState, Ctx, Event, Reject};
use crate::model::{Decision, HardshipCase, Hold, Posting, Reason, Relief, Role, TaskKind};

/// How long reminders wait for a hardship application after a referral.
pub const REFERRAL_DAYS: i64 = 14;

/// The longest hardship relief one decision may give.
pub const MAX_RELIEF_DAYS: i64 = 730;

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

fn plain(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl CollectionsState {
    /// Record what the customer said, and take the step the outcome calls
    /// for.
    pub(crate) fn record_outcome(&mut self, id: &str, reason: Reason, note: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let task = self.tasks.get(id).ok_or_else(|| bad("unknown_task", "no such task"))?;
        if task.closed.is_some() {
            return Err(bad("task_closed", "that task already has its outcome"));
        }
        if note.trim().is_empty() {
            return Err(bad("no_note", "an outcome says what was discussed"));
        }
        let (party, policies) = (task.party.clone(), task.policies.clone());
        ctx.emit(Event::OutcomeRecorded { task: id.to_string(), reason });
        if reason == Reason::NoContact {
            if let Some(task) = self.tasks.get_mut(id) {
                task.attempts += 1;
            }
            return Ok(());
        }
        if let Some(task) = self.tasks.get_mut(id) {
            task.closed = Some((ctx.today, reason));
        }
        self.open_tasks.remove(id);
        let hold = match reason {
            Reason::HardshipReferral => Some(Hold::HardshipReferral {
                until: ctx.today + Duration::days(REFERRAL_DAYS),
            }),
            Reason::Disputed => Some(Hold::Disputed),
            Reason::DeceasedNotified => Some(Hold::Deceased),
            _ => None,
        };
        let next = match reason {
            Reason::Reduction | Reason::CancellationRequested => Some(TaskKind::Servicing),
            Reason::Disputed => Some(TaskKind::Dispute),
            Reason::DeceasedNotified => Some(TaskKind::Deceased),
            _ => None,
        };
        for policy in &policies {
            if let (Some(hold), Some(p)) = (hold, self.policies.get_mut(policy)) {
                p.hold = Some(hold);
            }
            if let Some(kind) = next {
                self.open_task(&party, kind, policy, ctx);
            }
        }
        Ok(())
    }

    pub(crate) fn propose_arrangement(
        &mut self,
        policy: PolicyId,
        arrangement: String,
        instalments: Vec<ic_core::arrangement::Instalment>,
        ctx: &mut Ctx,
    ) -> Result<(), Reject> {
        let p = self.policies.get(&policy).ok_or_else(|| bad("unknown_policy", "no such policy"))?;
        if !self.in_arrears.contains(&policy) || p.ended {
            return Err(bad("not_in_arrears", "the policy has nothing overdue to arrange"));
        }
        if p.kept.is_some() {
            return Err(bad("arrangement_exists", "the policy already has an arrangement being kept"));
        }
        if !plain(&arrangement) {
            return Err(bad("invalid_arrangement", "an arrangement is named by a plain identifier"));
        }
        let sequence = self
            .sequences
            .get(&p.product)
            .ok_or_else(|| bad("no_sequence", "the product has no collections rules"))?;
        let count = u32::try_from(instalments.len()).unwrap_or(u32::MAX);
        let ordered = instalments.windows(2).all(|w| w[0].due < w[1].due);
        if count == 0 || !ordered || instalments.iter().any(|i| i.amount_minor <= 0 || i.due < ctx.today) {
            return Err(bad(
                "invalid_arrangement",
                "instalments are positive, from today, on different days in ascending order",
            ));
        }
        if count > sequence.max_arrangement_instalments && !ctx.roles.contains(&Role::TeamLead) {
            return Err(bad(
                "beyond_authority",
                format!(
                    "an officer may agree up to {} instalments; more needs the team lead",
                    sequence.max_arrangement_instalments
                ),
            ));
        }
        let id = format!("arrange:{policy}:{arrangement}");
        if self.outbox.contains_key(&id) {
            return Err(bad("arrangement_pending", "that arrangement is already with the core"));
        }
        let command = ic_core::Command::MakeArrangement {
            policy: policy.clone(),
            arrangement: arrangement.clone(),
            instalments,
        };
        self.post(id, &policy, command, ctx);
        ctx.emit(Event::ArrangementProposed {
            policy,
            arrangement,
            instalments: count,
        });
        Ok(())
    }

    pub(crate) fn end_arrangement(&mut self, policy: PolicyId, ctx: &mut Ctx) -> Result<(), Reject> {
        let kept = self.policies.get(&policy).and_then(|p| p.kept.clone());
        let kept = kept.ok_or_else(|| bad("no_arrangement", "the policy has no arrangement being kept"))?;
        let id = format!("end:{policy}:{}", kept.id);
        let command = ic_core::Command::EndArrangement {
            policy: policy.clone(),
            arrangement: kept.id,
        };
        self.post(id, &policy, command, ctx);
        Ok(())
    }

    /// Ask the core to reinstate a lapsed policy. Each request is its own
    /// posting, so a request after underwriting's approval is a new one.
    pub(crate) fn request_reinstatement(&mut self, policy: PolicyId, ctx: &mut Ctx) -> Result<(), Reject> {
        let p = self.policies.get(&policy).ok_or_else(|| bad("unknown_policy", "no such policy"))?;
        if !p.ended {
            return Err(bad("not_lapsed", "only a lapsed policy is reinstated"));
        }
        if self
            .outbox
            .values()
            .any(|o| o.policy == policy && matches!(o.command, ic_core::Command::Reinstate { .. }))
        {
            return Err(bad("reinstatement_pending", "the core has not answered the last request yet"));
        }
        let id = self.next("reinstate");
        self.post(id, &policy, ic_core::Command::Reinstate { policy: policy.clone() }, ctx);
        Ok(())
    }

    /// Underwriting approved the reinstatement the core sent it to: ask the
    /// core again, if collections asked first and is waiting for it (an open
    /// `ReinstatementUnderwriting` task names the policy). A second refusal,
    /// for arrears still unpaid say, becomes a reinstatement task as before.
    pub(crate) fn reinstatement_approved(&mut self, policy: PolicyId, ctx: &mut Ctx) {
        let waiting = self.open_tasks.iter().any(|id| {
            self.tasks
                .get(id)
                .is_some_and(|t| t.kind == TaskKind::ReinstatementUnderwriting && t.policies.contains(&policy))
        });
        if waiting {
            // A request already with the core, or a policy no longer lapsed,
            // needs nothing more.
            let _ = self.request_reinstatement(policy, ctx);
        }
    }

    /// Lapsed policies waiting for underwriting to take up their
    /// reinstatement, in order, each with the core partition it lives on.
    pub fn reinstatements_for_underwriting(&self) -> Vec<serde_json::Value> {
        let waiting: std::collections::BTreeMap<&PolicyId, u16> = self
            .open_tasks
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .filter(|t| t.kind == TaskKind::ReinstatementUnderwriting)
            .flat_map(|t| t.policies.iter())
            .filter_map(|p| self.policies.get(p).filter(|t| t.ended).map(|t| (p, t.partition)))
            .collect();
        waiting
            .into_iter()
            .map(|(policy, partition)| serde_json::json!({"policy": policy, "partition": partition}))
            .collect()
    }

    /// The core's answer to a posting. A refusal is a task: the officer
    /// tells the customer and agrees something else.
    pub(crate) fn core_outcome(&mut self, id: &str, applied: bool, code: Option<String>, message: Option<String>, ctx: &mut Ctx) -> Result<(), Reject> {
        let Some(posting) = self.outbox.remove(id) else {
            // Answered already: the outcome of a retry changes nothing.
            return Ok(());
        };
        if applied {
            return Ok(());
        }
        let kind = match posting.command {
            ic_core::Command::GrantHardship { .. } | ic_core::Command::EndHardship { .. } => TaskKind::HardshipDecision,
            ic_core::Command::Reinstate { .. } if code.as_deref() == Some("underwriting_required") => TaskKind::ReinstatementUnderwriting,
            ic_core::Command::Reinstate { .. } => TaskKind::Reinstatement,
            _ => TaskKind::Arrangement,
        };
        ctx.emit(Event::PostingRefused {
            policy: posting.policy.clone(),
            posting: posting.id,
            code: code.unwrap_or_default(),
            message: message.unwrap_or_default(),
        });
        if let Some(party) = self.policies.get(&posting.policy).map(|p| p.party.clone()) {
            self.open_task(&party, kind, &posting.policy, ctx);
        }
        Ok(())
    }

    pub(crate) fn apply_hardship(&mut self, case: HardshipCase, ctx: &mut Ctx) -> Result<(), Reject> {
        if !plain(&case.id) {
            return Err(bad("invalid_case", "a hardship case is named by a plain identifier"));
        }
        if let Some(existing) = self.hardship.get(&case.id) {
            return if existing.party == case.party && existing.policies == case.policies && existing.relief == case.relief {
                Ok(())
            } else {
                Err(bad("duplicate_case", "that case was opened with other details"))
            };
        }
        if case.policies.is_empty() || case.policies.iter().any(|p| self.policies.get(p).is_none_or(|t| t.party != case.party)) {
            return Err(bad("invalid_case", "a case covers one or more of the party's own policies"));
        }
        if case.from < ctx.today || case.to < case.from || (case.to - case.from).num_days() >= MAX_RELIEF_DAYS {
            return Err(bad(
                "invalid_case",
                format!("relief runs from today or later, for at most {MAX_RELIEF_DAYS} days"),
            ));
        }
        if let Relief::Reduced { percent } = case.relief {
            if percent == 0 || percent >= 100 {
                return Err(bad("invalid_case", "a reduced premium is between 1 and 99 per cent of the usual one"));
            }
        }
        if case.note.trim().is_empty() {
            return Err(bad("no_note", "an application says why relief is needed"));
        }
        let (party, first) = (case.party.clone(), case.policies[0].clone());
        // Reminders wait for the decision, as they do after a referral.
        let until = case.to;
        for policy in &case.policies {
            if let Some(p) = self.policies.get_mut(policy) {
                if p.hold.is_none_or(|h| !h.stops_notices()) {
                    p.hold = Some(Hold::HardshipReferral { until });
                }
            }
        }
        ctx.emit(Event::HardshipApplied {
            case: case.id.clone(),
            party: party.clone(),
        });
        self.hardship.insert(case.id.clone(), case);
        self.open_task(&party, TaskKind::HardshipDecision, &first, ctx);
        Ok(())
    }

    /// The hardship team decides, and never the person who applied.
    pub(crate) fn decide_hardship(&mut self, id: &str, approve: bool, note: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let case = self.hardship.get(id).ok_or_else(|| bad("unknown_case", "no such hardship case"))?;
        if case.decision.is_some() {
            return Err(bad("already_decided", "that case has its decision"));
        }
        if case.applied_by == ctx.actor {
            return Err(bad("same_person", "the person who applied cannot decide the case"));
        }
        if note.trim().is_empty() {
            return Err(bad("no_note", "a decision gives its reasons"));
        }
        let (party, policies) = (case.party.clone(), case.policies.clone());
        let case = case.clone();
        if let Some(case) = self.hardship.get_mut(id) {
            case.decision = Some(Decision {
                by: ctx.actor.clone(),
                on: ctx.today,
                approved: approve,
                note,
            });
        }
        let (relief, from, to) = (case.relief, case.from, case.to);
        for policy in &policies {
            if let Some(p) = self.policies.get_mut(policy) {
                if matches!(p.hold, Some(Hold::HardshipReferral { .. })) {
                    p.hold = None;
                }
                if approve {
                    p.relief = Some(id.to_string());
                }
            }
            if approve {
                // The core bills the share of premium still due for the
                // period, and credits what was already billed for it.
                let share_bps = match relief {
                    Relief::Holiday => 0,
                    Relief::Reduced { percent } => u32::from(percent) * 100,
                };
                self.post(
                    format!("grant:{policy}:{id}"),
                    policy,
                    ic_core::Command::GrantHardship {
                        policy: policy.clone(),
                        hardship: id.to_string(),
                        from,
                        to,
                        share_bps,
                    },
                    ctx,
                );
            }
        }
        let task = self
            .open_tasks
            .iter()
            .find(|t| self.tasks.get(*t).is_some_and(|t| t.party == party && t.kind == TaskKind::HardshipDecision))
            .cloned();
        if let Some(task) = task {
            self.open_tasks.remove(&task);
            if let Some(t) = self.tasks.get_mut(&task) {
                t.closed = Some((ctx.today, Reason::HardshipReferral));
            }
        }
        ctx.emit(Event::HardshipDecided {
            case: id.to_string(),
            approved: approve,
        });
        Ok(())
    }

    /// End relief early, today: normal billing resumes today, and nothing is
    /// charged for the relief already given.
    pub(crate) fn end_hardship(&mut self, id: &str, ctx: &mut Ctx) -> Result<(), Reject> {
        let case = self.hardship.get_mut(id).ok_or_else(|| bad("unknown_case", "no such hardship case"))?;
        let Some(last) = case.last_day() else {
            return Err(bad("not_approved", "only approved relief can be ended"));
        };
        let on = ctx.today;
        if on <= case.from || on > last {
            return Err(bad("invalid_end", "relief ends early on a day inside it, after its first"));
        }
        case.ended_early = Some(on);
        let (policies, case_id) = (case.policies.clone(), case.id.clone());
        for policy in &policies {
            self.post(
                format!("endrelief:{policy}:{case_id}"),
                policy,
                ic_core::Command::EndHardship {
                    policy: policy.clone(),
                    hardship: case_id.clone(),
                },
                ctx,
            );
        }
        ctx.emit(Event::HardshipEnded { case: case_id, on });
        Ok(())
    }

    /// Put a command for the core in the outbox, under the module's own key.
    fn post(&mut self, id: String, policy: &PolicyId, command: ic_core::Command, ctx: &Ctx) {
        self.outbox.insert(
            id.clone(),
            Posting {
                id,
                policy: policy.clone(),
                command,
                by: ctx.actor.clone(),
            },
        );
    }
}
