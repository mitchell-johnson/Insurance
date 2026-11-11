//! The partition state machine: command in, events and journal entries out.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_ledger::{EntryDraft, Hash32, JournalEntry, Ledger, LedgerError, PartitionId, Posting};
use ic_log::StateMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::Catalog;
use crate::command::{Command, Envelope};
use crate::event::{Event, EventRecord, OutboundMessage};
use crate::policy::{Policy, PolicyStatus};
use crate::premium::PremiumError;
use crate::{arrears, billing, cash, earning, group, lifecycle, transfer, unwind, PolicyId};

/// The apply budget: no single command may do more than this many units of
/// work in the state machine, because a partition applies commands one at a
/// time. A unit is roughly one posting, one layer rated or one member priced.
/// Exceeding it fails the simulation suite.
pub const APPLY_BUDGET: u32 = 4_000;

/// Largest chunk of policies one end-of-day command may carry.
pub const MAX_EOD_CHUNK: usize = 64;
/// Largest chunk of a membership file one command may carry.
pub const MAX_MEMBERSHIP_CHUNK: usize = 100;
/// Members priced per scheme billing step.
pub const SCHEME_BILLING_CHUNK: usize = 400;

/// A command the core refused. Nothing changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Reject {
    pub code: String,
    pub message: String,
}

impl Reject {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl From<PremiumError> for Reject {
    fn from(e: PremiumError) -> Self {
        Reject::new("premium", e.to_string())
    }
}
impl From<LedgerError> for Reject {
    fn from(e: LedgerError) -> Self {
        Reject::new("ledger", e.to_string())
    }
}
impl From<ic_money::MoneyError> for Reject {
    fn from(e: ic_money::MoneyError) -> Self {
        Reject::new("money", e.to_string())
    }
}

/// What applying one log entry produced. The host writes the events, entries
/// and messages to its outbox in the same atomic step as the state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub index: u64,
    pub result: Result<(), Reject>,
    pub events: Vec<EventRecord>,
    pub entries: Vec<JournalEntry>,
    pub messages: Vec<OutboundMessage>,
    pub work_units: u32,
    /// The key had been seen before: this is the original result.
    pub duplicate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredResult {
    index: u64,
    error: Option<Reject>,
}

/// Working context for one command.
pub(crate) struct Ctx<'a> {
    pub index: u64,
    pub date: NaiveDate,
    pub caller: &'a str,
    pub salt: &'a str,
    pub events: Vec<Event>,
    pub entries: Vec<JournalEntry>,
    pub messages: Vec<OutboundMessage>,
    pub work: u32,
}

impl Ctx<'_> {
    pub fn emit(&mut self, e: Event) {
        self.events.push(e);
    }

    /// Post a balanced entry. The entry and its event are produced together.
    pub fn post(
        &mut self,
        ledger: &mut Ledger,
        kind: &str,
        policy: Option<&Policy>,
        value_date: NaiveDate,
        postings: Vec<Posting>,
        detail: BTreeMap<String, String>,
    ) -> Result<(), Reject> {
        for p in &postings {
            debug_assert!(ledger.has_account(&p.account), "account {} not opened", p.account);
        }
        self.work += postings.len() as u32;
        let draft = EntryDraft {
            kind: kind.to_string(),
            value_date,
            postings,
            product_hash: policy.map(|p| p.product_hash.clone()),
            caller: self.caller.to_string(),
            policy: policy.map(|p| p.id.0.clone()),
            detail,
        };
        let salt = format!("{}:{}", self.salt, self.entries.len());
        let entry = ledger.post(draft, self.date, &salt)?;
        self.events.push(Event::EntryPosted {
            entry: entry.id,
            kind: kind.to_string(),
            policy: policy.map(|p| p.id.clone()),
            hash: entry.hash,
        });
        self.entries.push(entry);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreState {
    pub partition: PartitionId,
    pub business_date: Option<NaiveDate>,
    pub ledger: Ledger,
    pub catalog: Catalog,
    pub policies: BTreeMap<PolicyId, Policy>,
    next_event_seq: u64,
    idempotency: BTreeMap<String, StoredResult>,
    /// Transfers this partition has already landed or refused.
    pub(crate) transfers_seen: BTreeSet<String>,
    /// The last day the cutoff closed, with the ledger head at that moment.
    /// The end-of-day orchestration reads it to build the day's Merkle root,
    /// however many times it has to retry.
    pub last_close: Option<ClosedDay>,
}

/// A closed business day and the head of the ledger chain when it closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosedDay {
    pub date: NaiveDate,
    pub closing_head: Hash32,
}

impl CoreState {
    pub fn new(partition: PartitionId) -> Self {
        Self {
            partition,
            business_date: None,
            ledger: Ledger::new(partition),
            catalog: Catalog::default(),
            policies: BTreeMap::new(),
            next_event_seq: 1,
            idempotency: BTreeMap::new(),
            transfers_seen: BTreeSet::new(),
            last_close: None,
        }
    }

    pub fn last_event_seq(&self) -> u64 {
        self.next_event_seq - 1
    }

    /// Hash of the whole replicated state. Two replicas that applied the same
    /// log must agree on it exactly.
    pub fn state_hash(&self) -> Hash32 {
        let mut h = Sha256::new();
        h.update(b"ic.core.state.v1");
        h.update(self.ledger.digest().0);
        // serde_json over BTreeMaps and structs is a canonical encoding here.
        let rest = (
            &self.business_date,
            &self.catalog,
            &self.policies,
            self.next_event_seq,
            &self.idempotency,
            &self.transfers_seen,
            &self.last_close,
        );
        // Every type in the state has string or integer map keys, so this
        // cannot fail; a test holds that true by proving the hash moves with
        // state that is not in the ledger.
        h.update(serde_json::to_vec(&rest).unwrap_or_else(|_| b"unencodable".to_vec()));
        Hash32(h.finalize().into())
    }

    /// Policies an end-of-day run must visit.
    pub fn open_policy_ids(&self) -> Vec<PolicyId> {
        self.policies
            .values()
            .filter(|p| matches!(p.status, PolicyStatus::InForce | PolicyStatus::Lapsed))
            .map(|p| p.id.clone())
            .collect()
    }

    /// Policies the current day's end-of-day run has not finished with: a
    /// scheme invoice still being built, or a policy with a period still to
    /// bill. Empty once the run is complete.
    pub fn end_of_day_unfinished(&self) -> Vec<PolicyId> {
        let Some(today) = self.business_date else {
            return vec![];
        };
        self.policies
            .values()
            .filter(|p| {
                p.status == PolicyStatus::InForce
                    && !p.is_frozen()
                    && p.unwind.is_none()
                    && (p.pending_invoice.is_some()
                        || ic_product::schedule::period_start(p.inception, p.frequency, p.periods_billed).is_some_and(|s| s <= today))
            })
            .map(|p| p.id.clone())
            .collect()
    }

    fn dispatch(&mut self, ctx: &mut Ctx<'_>, command: Command) -> Result<(), Reject> {
        match command {
            Command::ActivateProduct { product } => {
                self.catalog.check_product(&product).map_err(config_conflict)?;
                ctx.emit(Event::ProductActivated {
                    code: product.code.clone(),
                    version: product.version,
                    content_hash: product.content_hash.clone(),
                });
                self.catalog.activate_product(*product);
                Ok(())
            }
            Command::ActivateRateTable { table } => {
                self.catalog.check_rate_table(&table).map_err(config_conflict)?;
                ctx.emit(Event::RateTableActivated {
                    id: table.id.clone(),
                    version: table.version,
                    content_hash: table.content_hash.clone(),
                    scoped: table.scope.is_some(),
                });
                self.catalog.activate_rate_table(*table);
                Ok(())
            }
            Command::ActivateDefinitions { definitions } => {
                self.catalog.check_definitions(&definitions).map_err(config_conflict)?;
                ctx.emit(Event::DefinitionsActivated {
                    count: definitions.len() as u32,
                });
                self.catalog.activate_definitions(definitions);
                Ok(())
            }
            Command::ActivateIndexRate { code, effective_from, rate } => {
                self.catalog.check_index_rate(&code, effective_from, &rate, ctx.date).map_err(config_conflict)?;
                ctx.emit(Event::IndexRateActivated {
                    code: code.clone(),
                    effective_from,
                });
                self.catalog.activate_index_rate(code, effective_from, rate);
                Ok(())
            }
            Command::OpenBusinessDate { .. } => Err(Reject::new("business_date", "the business date is already open")),
            Command::AdvanceBusinessDate { to } => {
                let from = ctx.date;
                if from.succ_opt() != Some(to) {
                    return Err(Reject::new(
                        "business_date",
                        format!("the business date is {from}; it can only advance one day"),
                    ));
                }
                self.business_date = Some(to);
                let closing_head = self.ledger.head();
                self.last_close = Some(ClosedDay { date: from, closing_head });
                ctx.emit(Event::BusinessDateAdvanced {
                    from: Some(from),
                    to,
                    closing_head,
                });
                Ok(())
            }
            Command::RunEndOfDay { policies } => {
                if policies.len() > MAX_EOD_CHUNK {
                    return Err(Reject::new("apply_budget", format!("at most {MAX_EOD_CHUNK} policies per end-of-day command")));
                }
                for id in policies {
                    // One policy's failure must not stop the chunk: it becomes
                    // an exception item and the rest of the book carries on.
                    if let Err(e) = self.with_policy_tracked(ctx, &id, false, |s, ctx, p| s.end_of_day(ctx, p)) {
                        ctx.emit(Event::PolicyException {
                            policy: id,
                            code: e.code,
                            message: e.message,
                        });
                    }
                }
                Ok(())
            }
            Command::IssuePolicy(cmd) => {
                let id = cmd.policy.clone();
                let r = lifecycle::issue(self, ctx, *cmd);
                if let Some(p) = self.policies.get(&id).filter(|_| r.is_ok()) {
                    ctx.emit(Event::PolicyBalance {
                        policy: id,
                        outstanding_minor: p.outstanding_minor(),
                        oldest_unpaid_due: p.oldest_unpaid_due(),
                    });
                }
                r
            }
            Command::CancelPolicy { policy } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| lifecycle::cancel(&mut s.ledger, &s.catalog, p, ctx)),
            Command::Reinstate { policy } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| lifecycle::reinstate(&mut s.ledger, &s.catalog, p, ctx)),
            Command::DeclineIndexation { policy, declined } => self.with_policy(&policy, true, |_, p| {
                p.indexation_declined = declined;
                ctx.emit(Event::IndexationChoiceRecorded {
                    policy: p.id.clone(),
                    declined,
                });
                Ok(())
            }),
            Command::MergeLayers { policy, cover } => self.with_policy(&policy, true, |_, p| lifecycle::merge_layers(p, &cover, ctx)),
            Command::ApplyMembership { policy, changes } => {
                if changes.len() > MAX_MEMBERSHIP_CHUNK {
                    return Err(Reject::new(
                        "apply_budget",
                        format!("at most {MAX_MEMBERSHIP_CHUNK} membership changes per command"),
                    ));
                }
                self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| {
                    group::apply_membership(&mut s.ledger, &s.catalog, p, ctx, changes)
                })
            }
            Command::IdentifyMember { policy, life, party } => self.with_policy(&policy, true, |_, p| group::identify(p, ctx, &life, party)),
            Command::ApplyReceipt { policy, receipt, amount } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| {
                cash::apply_receipt(&mut s.ledger, &s.catalog, p, ctx, receipt, amount)
            }),
            Command::ReverseReceipt { policy, receipt } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| {
                cash::reverse_receipt(&mut s.ledger, &s.catalog, p, ctx, &receipt)
            }),
            Command::RefundSuspense { policy, amount, with_claim } => {
                self.with_policy(&policy, true, |s, p| cash::refund_suspense(&mut s.ledger, p, ctx, amount, with_claim))
            }
            Command::UnwindAfter {
                policy,
                after,
                cause,
                reference,
            } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| {
                unwind::unwind_after(&mut s.ledger, &s.catalog, p, ctx, after, cause, &reference)
            }),
            Command::ResumeAfterUnwind { policy, reference } => self.with_policy(&policy, true, |_, p| unwind::resume(p, ctx, &reference)),
            Command::TerminateOnClaim {
                policy,
                event_date,
                claim,
                payer_deceased,
            } => self.with_policy_tracked(ctx, &policy, true, |s, ctx, p| {
                unwind::terminate_on_claim(&mut s.ledger, &s.catalog, p, ctx, event_date, &claim, payer_deceased)
            }),
            Command::BeginTransfer {
                policy,
                transfer_id,
                target_group,
                target_partition,
            } => self.with_policy(&policy, true, |s, p| {
                transfer::begin(&mut s.ledger, &s.catalog, p, ctx, transfer_id, target_group, target_partition)
            }),
            Command::LandTransfer { record } => {
                let id = record.policy.id.clone();
                let r = transfer::land(self, ctx, *record);
                if let Some(p) = self.policies.get(&id).filter(|p| r.is_ok() && p.status != PolicyStatus::Transferred) {
                    ctx.emit(Event::PolicyBalance {
                        policy: id,
                        outstanding_minor: p.outstanding_minor(),
                        oldest_unpaid_due: p.oldest_unpaid_due(),
                    });
                }
                r
            }
            Command::CompleteTransfer { policy, transfer_id } => self.with_policy(&policy, false, |_, p| transfer::complete(p, ctx, &transfer_id)),
            Command::AbortTransfer { policy, transfer_id } => self.with_policy(&policy, false, |s, p| transfer::abort(&mut s.ledger, p, ctx, &transfer_id)),
        }
    }

    /// Run a handler on one policy. The policy is taken out of the map so the
    /// handler can borrow the ledger and catalog freely, and is always put
    /// back. Handlers post first and mutate only once the posting has
    /// succeeded, so a rejected command leaves nothing behind.
    fn with_policy<F>(&mut self, id: &PolicyId, refuse_if_frozen: bool, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&mut Self, &mut Policy) -> Result<(), Reject>,
    {
        let mut policy = self
            .policies
            .remove(id)
            .ok_or_else(|| Reject::new("unknown_policy", format!("policy {id} is not on this partition")))?;
        let r = if refuse_if_frozen && policy.is_frozen() {
            Err(Reject::new(
                "policy_transferring",
                format!("policy {id} is being transferred; retry after it lands"),
            ))
        } else if refuse_if_frozen && policy.status == PolicyStatus::Transferred {
            Err(Reject::new("policy_transferred", format!("policy {id} has moved to another group")))
        } else {
            f(self, &mut policy)
        };
        self.policies.insert(id.clone(), policy);
        r
    }

    /// Like [`Self::with_policy`], and reports the policy's balance if the
    /// handler changed it.
    fn with_policy_tracked<F>(&mut self, ctx: &mut Ctx<'_>, id: &PolicyId, refuse_if_frozen: bool, f: F) -> Result<(), Reject>
    where
        F: FnOnce(&mut Self, &mut Ctx<'_>, &mut Policy) -> Result<(), Reject>,
    {
        let before = self.policies.get(id).map(|p| (p.outstanding_minor(), p.oldest_unpaid_due()));
        let r = self.with_policy(id, refuse_if_frozen, |s, p| f(s, ctx, p));
        if let Some(p) = self.policies.get(id) {
            let after = (p.outstanding_minor(), p.oldest_unpaid_due());
            if before != Some(after) && p.status != PolicyStatus::Transferred {
                ctx.emit(Event::PolicyBalance {
                    policy: id.clone(),
                    outstanding_minor: after.0,
                    oldest_unpaid_due: after.1,
                });
            }
        }
        r
    }

    /// End-of-day for one policy, in the defined order: anniversary, billing,
    /// earning for the closed day, then arrears. Every step catches up from
    /// where the policy's own state says it got to, so a rerun, a late run or
    /// a run after a transfer changes nothing it should not.
    fn end_of_day(&mut self, ctx: &mut Ctx<'_>, policy: &mut Policy) -> Result<(), Reject> {
        if policy.is_frozen() || policy.status != PolicyStatus::InForce {
            return Ok(());
        }
        let product = self
            .catalog
            .product(&policy.product_hash)
            .ok_or_else(|| Reject::new("unknown_product", "the policy's product version is not active on this partition"))?
            .clone();
        if policy.unwind.is_some() {
            // Held as at the unwind date: what is still billed goes on
            // earning, and nothing that was just reversed is applied again.
            if let Some(closed_day) = ctx.date.pred_opt() {
                earning::earn_through(&mut self.ledger, policy, ctx, closed_day)?;
            }
            return Ok(());
        }
        lifecycle::anniversaries(&self.catalog, &product, policy, ctx)?;
        if policy.status != PolicyStatus::InForce {
            // The policy expired at this anniversary. The day just closed was
            // still a day of cover, so it is earned before the policy rests.
            if let Some(closed_day) = ctx.date.pred_opt() {
                earning::earn_through(&mut self.ledger, policy, ctx, closed_day)?;
            }
            return Ok(());
        }
        billing::bill_due_periods(&mut self.ledger, &self.catalog, &product, policy, ctx)?;
        if let Some(closed_day) = ctx.date.pred_opt() {
            earning::earn_through(&mut self.ledger, policy, ctx, closed_day)?;
        }
        arrears::run(&mut self.ledger, &product, policy, ctx)?;
        if let Some(keep_from) = ctx.date.checked_sub_days(chrono::Days::new(400)) {
            policy.prune_settled(keep_from);
        }
        Ok(())
    }

    /// Apply one logged command.
    pub fn apply_envelope(&mut self, index: u64, env: Envelope) -> Applied {
        if let Some(stored) = self.idempotency.get(&env.key) {
            return Applied {
                index: stored.index,
                result: stored.error.clone().map_or(Ok(()), Err),
                events: vec![],
                entries: vec![],
                messages: vec![],
                work_units: 0,
                duplicate: true,
            };
        }
        let mut ctx_date = self.business_date;
        let mut pre: Result<(), Reject> = Ok(());
        if !env.command.allowed_callers().contains(&env.caller.as_str()) {
            pre = Err(Reject::new(
                "caller_not_allowed",
                format!("module {} may not send {}", env.caller, env.command.name()),
            ));
        } else if let Command::OpenBusinessDate { date } = &env.command {
            if self.business_date.is_none() {
                self.business_date = Some(*date);
                ctx_date = Some(*date);
            }
        }
        let date = match (ctx_date, &pre) {
            (Some(d), _) => d,
            (None, Ok(())) => {
                pre = Err(Reject::new("business_date", "the partition has no business date yet"));
                NaiveDate::MIN
            }
            (None, Err(_)) => NaiveDate::MIN,
        };
        let mut ctx = Ctx {
            index,
            date,
            caller: &env.caller,
            salt: &env.salt,
            events: vec![],
            entries: vec![],
            messages: vec![],
            work: 1,
        };
        let result = match pre {
            Err(e) => Err(e),
            Ok(()) => match env.command {
                Command::OpenBusinessDate { date } if self.business_date == Some(date) && self.ledger.entry_count() == 0 && self.next_event_seq == 1 => {
                    ctx.emit(Event::BusinessDateAdvanced {
                        from: None,
                        to: date,
                        closing_head: self.ledger.head(),
                    });
                    Ok(())
                }
                other => self.dispatch(&mut ctx, other),
            },
        };
        let partition = self.partition;
        let events: Vec<EventRecord> = ctx
            .events
            .into_iter()
            .map(|event| {
                let seq = self.next_event_seq;
                self.next_event_seq += 1;
                EventRecord {
                    id: format!("{partition}-{seq}"),
                    partition,
                    seq,
                    business_date: date,
                    log_index: index,
                    traceparent: env.traceparent.clone(),
                    event,
                }
            })
            .collect();
        self.idempotency.insert(
            env.key,
            StoredResult {
                index,
                error: result.clone().err(),
            },
        );
        Applied {
            index,
            result,
            events,
            entries: ctx.entries,
            messages: ctx.messages,
            work_units: ctx.work,
            duplicate: false,
        }
    }
}

impl StateMachine for CoreState {
    type Command = Envelope;
    type Output = Applied;
    fn apply(&mut self, index: u64, command: Envelope) -> Applied {
        self.apply_envelope(index, command)
    }
}

/// Active configuration is immutable; see the checks in the catalog.
fn config_conflict(message: String) -> Reject {
    Reject::new("config_conflict", message)
}
