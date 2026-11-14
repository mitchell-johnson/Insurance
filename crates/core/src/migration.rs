//! Loading policies written on another system, one cohort at a time.
//!
//! There is no back door: a migrated policy is built by the same code as a
//! policy issued here, from the same issue command, and its money is posted
//! by entries like any other. What is different is said outright:
//!
//! - **A cohort** is opened with the day it cuts over and the old system's
//!   control totals. Policies are loaded into it on that business day.
//! - **Each load** carries the issue command and the policy's opening
//!   position: how many periods the old system billed, the bills still
//!   owed or still earning with their original due dates, money held, and
//!   what had to be assumed where the old record was silent. Nothing is
//!   billed again and nothing is announced.
//! - **Opening balances** post against the cohort's clearing account:
//!   what is owed, what is unearned on the cutover day by this system's own
//!   daily rule, and money held.
//! - **Sign-off** is refused unless what was loaded equals the control
//!   totals, to the cent and to the count. It posts the control totals the
//!   other way, so the clearing account returns to zero, and records who
//!   signed.
//! - **Release** then announces the policies, in bounded steps. Until a
//!   policy is released no other command reaches it, and the business day
//!   does not advance while a cohort is neither released nor abandoned.
//! - **Abandon**, before sign-off, takes the load out again entry by entry.
//!   Nothing was announced, so nothing changed for anyone.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_money::{Currency, FINE_PER_MINOR};
use ic_product::schedule::policy_year;
use serde::{Deserialize, Serialize};

use crate::accounts::{self, dims};
use crate::command::IssuePolicy;
use crate::earning::earned_target;
use crate::event::Event;
use crate::hardship::HardshipOverride;
use crate::machine::{CoreState, Ctx, Reject};
use crate::policy::{LayerOrigin, Policy, Receivable, ReceivableKind, Standing};
use crate::postings::{div_half_even, PostingSet};
use crate::PolicyId;

/// The most policies one release or abandon step works through.
pub const MAX_COHORT_STEP: u32 = 200;

/// Figures a cohort is reconciled on. The old system's are given when the
/// cohort is opened; this system's are added up as policies load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlTotals {
    pub policies: u32,
    pub lives: u32,
    /// Individually written amounts of cover: layers.
    pub layers: u32,
    /// Sum insured in force on the cutover day.
    pub sum_insured_minor: i64,
    /// The bill for the period each policy is in on the cutover day, as
    /// each system prices it.
    pub period_premium_minor: i64,
    /// Premium billed and not paid.
    pub receivable_minor: i64,
    /// Premium billed for days from the cutover day on.
    pub unearned_minor: i64,
    /// Money held and not applied.
    pub suspense_minor: i64,
}

impl ControlTotals {
    /// Each figure that differs from `other`, named, as (name, this, other).
    pub fn differences(&self, other: &ControlTotals) -> Vec<(&'static str, i64, i64)> {
        [
            ("policies", self.policies as i64, other.policies as i64),
            ("lives", self.lives as i64, other.lives as i64),
            ("layers", self.layers as i64, other.layers as i64),
            ("sum_insured_minor", self.sum_insured_minor, other.sum_insured_minor),
            ("period_premium_minor", self.period_premium_minor, other.period_premium_minor),
            ("receivable_minor", self.receivable_minor, other.receivable_minor),
            ("unearned_minor", self.unearned_minor, other.unearned_minor),
            ("suspense_minor", self.suspense_minor, other.suspense_minor),
        ]
        .into_iter()
        .filter(|(_, a, b)| a != b)
        .collect()
    }
    fn add(&mut self, o: &ControlTotals) {
        self.policies += o.policies;
        self.lives += o.lives;
        self.layers += o.layers;
        self.sum_insured_minor += o.sum_insured_minor;
        self.period_premium_minor += o.period_premium_minor;
        self.receivable_minor += o.receivable_minor;
        self.unearned_minor += o.unearned_minor;
        self.suspense_minor += o.suspense_minor;
    }
    /// What the cohort brings onto the ledger, net: a liability if positive.
    fn net_minor(&self) -> i128 {
        self.unearned_minor as i128 + self.suspense_minor as i128 - self.receivable_minor as i128
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CohortStatus {
    /// Open for loading on its cutover day.
    Loading,
    /// Reconciled and signed; its policies are being announced.
    SignedOff,
    /// Every policy announced. The cohort is live.
    Released,
    /// Being taken out again.
    Abandoning,
    Abandoned,
}

/// Who signed a cohort off, and the evidence they signed on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignOff {
    pub finance: String,
    pub business_owner: String,
    /// The appointed actuary, where the cohort touches reserves or factors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actuary: Option<String>,
    /// The reconciliation pack in the vault, by its hash.
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cohort {
    pub currency: Currency,
    pub cutover: NaiveDate,
    pub status: CohortStatus,
    /// The old system's figures.
    pub control: ControlTotals,
    /// What has been loaded.
    pub loaded: ControlTotals,
    /// Policies loaded and not yet announced or taken out.
    pub waiting: BTreeSet<PolicyId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign_off: Option<SignOff>,
}

/// Every cohort this partition has seen, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationBook {
    pub cohorts: BTreeMap<String, Cohort>,
}

impl MigrationBook {
    pub fn is_empty(&self) -> bool {
        self.cohorts.is_empty()
    }
    /// The cohort a policy is still held in, if any: loaded and not yet
    /// announced.
    pub fn holding<'a>(&self, policy: &'a Policy) -> Option<&'a str> {
        let record = policy.migration.as_ref()?;
        let cohort = self.cohorts.get(&record.cohort)?;
        cohort.waiting.contains(&policy.id).then_some(record.cohort.as_str())
    }
    /// A cohort that stops the business day advancing: still loading, being
    /// released or being taken out.
    pub fn open_cohort(&self) -> Option<&str> {
        self.cohorts
            .iter()
            .find(|(_, c)| matches!(c.status, CohortStatus::Loading | CohortStatus::SignedOff | CohortStatus::Abandoning))
            .map(|(name, _)| name.as_str())
    }
}

/// What a migrated policy keeps of its load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub cohort: String,
    pub cutover: NaiveDate,
    /// What was assumed where the old record was silent, as codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

/// A bill from the old system that is still owed or still earning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpeningBill {
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// The day it fell due on the old system. Standing is counted from it.
    pub due: NaiveDate,
    /// Premium by cover type, and `policy_fee`, before tax and levies.
    pub net: BTreeMap<String, i64>,
    #[serde(default)]
    pub gst_minor: i64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub levies: BTreeMap<String, i64>,
    /// What of it is still unpaid.
    pub outstanding_minor: i64,
}

impl OpeningBill {
    fn total(&self) -> i64 {
        self.net.values().sum::<i64>() + self.gst_minor + self.levies.values().sum::<i64>()
    }
}

/// A policy's position on the cutover day.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opening {
    /// Billing periods the old system billed, counted from inception.
    pub periods_billed: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bills: Vec<OpeningBill>,
    #[serde(default)]
    pub suspense_minor: i64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub indexation_declined: bool,
    /// A premium holiday or reduced premium granted on the old system, with
    /// its original dates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardship: Option<HardshipOverride>,
    /// The day each cover in the issue command began, in the command's
    /// order, where the old system kept the history of increases. Empty
    /// where it kept only today's amount: every layer then starts at
    /// inception and the load must say so in `assumptions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layer_starts: Vec<NaiveDate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
}

/// One policy to load: the ordinary issue command, marked as migrated, and
/// its opening position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Load {
    pub cohort: String,
    pub issue: IssuePolicy,
    pub opening: Opening,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum MigrationCommand {
    OpenCohort {
        cohort: String,
        currency: Currency,
        cutover: NaiveDate,
        control: ControlTotals,
    },
    Load(Box<Load>),
    SignOff {
        cohort: String,
        sign_off: SignOff,
    },
    /// Announce up to `limit` of a signed-off cohort's policies.
    Release {
        cohort: String,
        limit: u32,
    },
    /// Take up to `limit` policies of a cohort out again. Before sign-off only.
    Abandon {
        cohort: String,
        limit: u32,
    },
}

fn named(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn bad(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

pub(crate) fn execute(state: &mut CoreState, ctx: &mut Ctx<'_>, command: MigrationCommand) -> Result<(), Reject> {
    match command {
        MigrationCommand::OpenCohort {
            cohort,
            currency,
            cutover,
            control,
        } => open(state, ctx, cohort, currency, cutover, control),
        MigrationCommand::Load(load) => self::load(state, ctx, *load),
        MigrationCommand::SignOff { cohort, sign_off } => self::sign_off(state, ctx, &cohort, sign_off),
        MigrationCommand::Release { cohort, limit } => release(state, ctx, &cohort, limit),
        MigrationCommand::Abandon { cohort, limit } => abandon(state, ctx, &cohort, limit),
    }
}

fn open(state: &mut CoreState, ctx: &mut Ctx<'_>, name: String, currency: Currency, cutover: NaiveDate, control: ControlTotals) -> Result<(), Reject> {
    if !named(&name) {
        return Err(bad("invalid_cohort", "a cohort has a plain name"));
    }
    if let Some(existing) = state.migration.cohorts.get(&name) {
        return if (existing.currency, existing.cutover, existing.control) == (currency, cutover, control) {
            Ok(())
        } else {
            Err(bad("cohort_exists", format!("cohort {name} was opened with other figures")))
        };
    }
    if cutover != ctx.date {
        return Err(bad(
            "cutover_date",
            format!("a cohort is opened on its cutover day; the business date is {}", ctx.date),
        ));
    }
    if control.receivable_minor < 0 || control.unearned_minor < 0 || control.suspense_minor < 0 || control.sum_insured_minor < 0 {
        return Err(bad("invalid_cohort", "control totals are not negative"));
    }
    state.migration.cohorts.insert(
        name.clone(),
        Cohort {
            currency,
            cutover,
            status: CohortStatus::Loading,
            control,
            loaded: ControlTotals::default(),
            waiting: BTreeSet::new(),
            sign_off: None,
        },
    );
    ctx.emit(Event::CohortOpened { cohort: name, cutover });
    Ok(())
}

/// The policy's own share of the cohort's figures.
fn figures(policy: &Policy, cutover: NaiveDate, period_premium_minor: i64) -> ControlTotals {
    let layers = || policy.covers.values().flat_map(|c| &c.layers);
    ControlTotals {
        policies: 1,
        lives: policy.lives.len() as u32,
        layers: layers().count() as u32,
        sum_insured_minor: layers().filter(|l| l.in_force_on(cutover)).map(|l| l.sum_insured_minor).sum(),
        period_premium_minor,
        receivable_minor: policy.outstanding_minor(),
        unearned_minor: div_half_even(policy.unearned_fine(), FINE_PER_MINOR) as i64,
        suspense_minor: 0,
    }
}

fn load(state: &mut CoreState, ctx: &mut Ctx<'_>, load: Load) -> Result<(), Reject> {
    let Load { cohort: name, issue, opening } = load;
    let cohort = state
        .migration
        .cohorts
        .get(&name)
        .ok_or_else(|| bad("unknown_cohort", format!("cohort {name} is not open on this partition")))?;
    if cohort.status != CohortStatus::Loading {
        return Err(bad("cohort_closed", format!("cohort {name} is no longer loading")));
    }
    let cutover = cohort.cutover;
    if ctx.date != cutover {
        return Err(bad("cutover_date", "a cohort is loaded on its cutover day"));
    }
    if state.policies.contains_key(&issue.policy) {
        return Err(bad("policy_exists", format!("policy {} already exists", issue.policy)));
    }
    if !issue.migrated || issue.continuation_of.is_some() {
        return Err(bad("migrated", "a load carries an issue command marked as migrated and nothing else"));
    }
    if opening.assumptions.iter().any(|a| !named(a)) {
        return Err(bad("invalid_opening", "an assumption is recorded by its code"));
    }
    if !opening.layer_starts.is_empty() && opening.layer_starts.len() != issue.covers.len() {
        return Err(bad("invalid_opening", "layer starts are given for every cover in the command, or for none"));
    }
    if opening.layer_starts.is_empty() && issue.inception < cutover && !opening.assumptions.iter().any(|a| a == "single_layer") {
        return Err(bad(
            "invalid_opening",
            "without the history of increases every layer starts at inception, and the load records that as the assumption single_layer",
        ));
    }
    let covers = issue.covers.clone();
    let (mut policy, product) = crate::lifecycle::build_policy(&state.catalog, issue, ctx.date, ctx.index)?;
    if policy.currency != cohort.currency {
        return Err(bad("currency", "the policy is not in the cohort's currency"));
    }
    // Each layer's own first day. The command's covers became layers in
    // order, numbered within their cover.
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (spec, start) in covers.iter().zip(&opening.layer_starts) {
        let nth = seen.entry(spec.code.as_str()).or_insert(0);
        let Some(layer) = policy.covers.get_mut(&spec.code).and_then(|c| c.layers.get_mut(*nth)) else {
            continue;
        };
        *nth += 1;
        if *start < policy.inception || *start > cutover {
            return Err(bad("invalid_opening", "a layer starts between inception and the cutover day"));
        }
        if *start > policy.inception {
            layer.start = *start;
            layer.origin = LayerOrigin::Increase;
        }
    }
    // Billing carries on from where the old system stopped: it billed every
    // period that began before the cutover day, and this system bills the
    // rest.
    policy.periods_billed = opening.periods_billed;
    let next = policy.period_start(opening.periods_billed);
    if next.is_none_or(|next| next < cutover) {
        return Err(bad(
            "opening_billing",
            "the old system billed every period that began before the cutover day; a policy it left unbilled is an exception to clear first",
        ));
    }
    let Some(day_before) = cutover.pred_opt() else {
        return Err(bad("date", "cutover out of range"));
    };
    policy.anniversaries_processed = if cutover > policy.inception {
        policy_year(policy.inception, day_before)
    } else {
        0
    };
    policy.indexation_declined = opening.indexation_declined;
    if let Some(o) = &opening.hardship {
        if !named(&o.id) || o.from >= o.to || o.share_bps >= 10_000 {
            return Err(bad(
                "invalid_opening",
                "a hardship override has a name, a first day before its last and a reduced share",
            ));
        }
        policy.hardship = Some(o.clone());
    }
    if opening.suspense_minor < 0 {
        return Err(bad("invalid_opening", "money held is not negative"));
    }

    // The opening entry: what is owed, what is unearned from the cutover
    // day by this system's daily rule in whole cents, and money held, each
    // against the cohort's clearing account.
    let c = policy.currency;
    let clearing = accounts::migration_clearing(&name, c);
    let mut set = PostingSet::new();
    let mut receivables = Vec::new();
    for bill in &opening.bills {
        let total = bill.total();
        let in_order = bill.period_start < bill.period_end && next.is_some_and(|next| bill.period_end <= next);
        let amounts = bill.net.values().all(|v| *v >= 0) && bill.gst_minor >= 0 && bill.levies.values().all(|v| *v >= 0);
        if !in_order || !amounts || bill.outstanding_minor < 0 || bill.outstanding_minor > total {
            return Err(bad(
                "invalid_opening",
                "an opening bill is for a period the old system billed, with amounts that are not negative and no more owing than it billed",
            ));
        }
        if bill.outstanding_minor == 0 && bill.period_end <= cutover {
            return Err(bad("invalid_opening", "a bill that is paid and fully earned is history; it is not loaded"));
        }
        let mut r = Receivable {
            id: policy.next_receivable_id,
            kind: ReceivableKind::Period,
            period_start: bill.period_start,
            period_end: bill.period_end,
            due: bill.due,
            total_minor: total,
            outstanding_minor: bill.outstanding_minor,
            gst_minor: bill.gst_minor,
            levies_minor: bill.levies.clone(),
            net_fine: bill.net.iter().map(|(k, v)| (k.clone(), *v as i128 * FINE_PER_MINOR)).collect(),
            earned_fine: BTreeMap::new(),
            closed: false,
            payer: None,
            entry: None,
            settled_on: None,
        };
        policy.next_receivable_id += 1;
        let earned = earned_target(&r, day_before);
        for (cover_type, net) in &r.net_fine {
            let unearned_minor = div_half_even(net - earned.get(cover_type).copied().unwrap_or(0), FINE_PER_MINOR);
            r.earned_fine.insert(cover_type.clone(), net - unearned_minor * FINE_PER_MINOR);
            set.credit(
                accounts::unearned_premium(&policy.id, c),
                unearned_minor * FINE_PER_MINOR,
                dims(&policy, Some(cover_type), "premium"),
            );
            set.debit(clearing.clone(), unearned_minor, dims(&policy, None, "migration"));
        }
        set.debit(
            accounts::premium_receivable(&policy.id, c),
            bill.outstanding_minor as i128,
            dims(&policy, None, "premium"),
        );
        set.credit(clearing.clone(), bill.outstanding_minor as i128, dims(&policy, None, "migration"));
        receivables.push(r);
    }
    set.credit(
        accounts::policy_suspense(&policy.id, c),
        opening.suspense_minor as i128,
        dims(&policy, None, "premium"),
    );
    set.debit(clearing, opening.suspense_minor as i128, dims(&policy, None, "migration"));
    policy.receivables = receivables;

    // Priced as this system will bill it, before anything is written: the
    // period the policy is in on the cutover day, or its first.
    let priced_from = opening
        .periods_billed
        .checked_sub(1)
        .and_then(|n| policy.period_start(n))
        .unwrap_or(policy.inception);
    let period_premium = if policy.is_scheme() {
        0
    } else {
        crate::premium::retail_period_premium(&state.catalog, &product, &policy, priced_from, &mut ctx.work)?.total()
    };

    for a in accounts::policy_accounts(&policy.id, c) {
        state.ledger.open_account(a)?;
    }
    if !set.is_empty() {
        let mut detail = BTreeMap::new();
        detail.insert("cohort".to_string(), name.clone());
        let postings = set.into_postings(&mut state.ledger)?;
        ctx.post(&mut state.ledger, "migration_opening", Some(&policy), cutover, postings, detail)?;
    }
    for r in &mut policy.receivables {
        r.entry = ctx.entries.last().map(|e| e.id);
    }
    // Standing as the bills' own due dates make it, with nothing announced:
    // the old system already told the customer.
    if let Some(rules) = product.arrears() {
        policy.standing = match crate::arrears::derive(&policy, rules, cutover) {
            crate::arrears::Derived::Standing(s) => Some(s),
            // Overdue to lapse: the first end of day here lapses it, with its notice.
            crate::arrears::Derived::Lapse => Some(Standing::Suspended),
        };
    }
    policy.migration = Some(MigrationRecord {
        cohort: name.clone(),
        cutover,
        assumptions: opening.assumptions,
    });
    policy.stamp_settled(ctx.date);
    let mut mine = figures(&policy, cutover, period_premium);
    mine.suspense_minor = opening.suspense_minor;
    if let Some(cohort) = state.migration.cohorts.get_mut(&name) {
        cohort.loaded.add(&mine);
        cohort.waiting.insert(policy.id.clone());
    }
    state.note_holder(&policy);
    state.policies.insert(policy.id.clone(), policy);
    Ok(())
}

fn cohort_in<'a>(state: &'a mut CoreState, name: &str, status: &[CohortStatus]) -> Result<&'a mut Cohort, Reject> {
    let cohort = state
        .migration
        .cohorts
        .get_mut(name)
        .ok_or_else(|| bad("unknown_cohort", format!("cohort {name} is not open on this partition")))?;
    if !status.contains(&cohort.status) {
        return Err(bad("cohort_closed", format!("cohort {name} is {:?}", cohort.status)));
    }
    Ok(cohort)
}

fn sign_off(state: &mut CoreState, ctx: &mut Ctx<'_>, name: &str, signed: SignOff) -> Result<(), Reject> {
    if let Some(cohort) = state.migration.cohorts.get(name) {
        if cohort.sign_off.as_ref() == Some(&signed) {
            return Ok(());
        }
    }
    let cohort = &*cohort_in(state, name, &[CohortStatus::Loading])?;
    let people = [Some(&signed.finance), Some(&signed.business_owner), signed.actuary.as_ref()];
    let people: Vec<&String> = people.into_iter().flatten().collect();
    let distinct: BTreeSet<&String> = people.iter().copied().collect();
    if people.iter().any(|p| !named(p)) || distinct.len() != people.len() || signed.evidence.is_empty() {
        return Err(bad(
            "invalid_sign_off",
            "a cohort is signed off by finance and the business owner, and the actuary where needed, each a different person, on named evidence",
        ));
    }
    let differences = cohort.loaded.differences(&cohort.control);
    if !differences.is_empty() {
        let listed: Vec<String> = differences
            .iter()
            .map(|(what, loaded, control)| format!("{what}: loaded {loaded}, control {control}"))
            .collect();
        return Err(bad("control_totals", format!("cohort {name} does not reconcile ({})", listed.join("; "))));
    }
    // The old system's totals, the other way: the clearing account returns
    // to zero and the control account carries what came across.
    let (c, net, cutover) = (cohort.currency, cohort.control.net_minor(), cohort.cutover);
    let (policies, control, released) = (cohort.loaded.policies, cohort.control, cohort.waiting.is_empty());
    let held = state.ledger.balance(&accounts::migration_clearing(name, c).id);
    if held != net {
        return Err(bad(
            "clearing_not_zero",
            format!("cohort {name}'s clearing account holds {held} where its control totals come to {net}"),
        ));
    }
    if net != 0 {
        let mut set = PostingSet::new();
        set.credit(accounts::migration_clearing(name, c), net, Default::default());
        set.debit(accounts::policy_migration_control(c), net, Default::default());
        let mut detail = BTreeMap::new();
        detail.insert("cohort".to_string(), name.to_string());
        let postings = set.into_postings(&mut state.ledger)?;
        ctx.post(&mut state.ledger, "migration_control", None, cutover, postings, detail)?;
    }
    if let Some(cohort) = state.migration.cohorts.get_mut(name) {
        cohort.status = if released { CohortStatus::Released } else { CohortStatus::SignedOff };
        cohort.sign_off = Some(signed.clone());
    }
    ctx.emit(Event::CohortSignedOff {
        cohort: name.to_string(),
        cutover,
        policies,
        control,
        sign_off: signed,
    });
    Ok(())
}

fn release(state: &mut CoreState, ctx: &mut Ctx<'_>, name: &str, limit: u32) -> Result<(), Reject> {
    if limit == 0 || limit > MAX_COHORT_STEP {
        return Err(bad("apply_budget", format!("release between 1 and {MAX_COHORT_STEP} policies a step")));
    }
    if state.migration.cohorts.get(name).is_some_and(|c| c.status == CohortStatus::Released) {
        return Ok(());
    }
    let cohort = cohort_in(state, name, &[CohortStatus::SignedOff])?;
    let step: Vec<PolicyId> = cohort.waiting.iter().take(limit as usize).cloned().collect();
    for id in &step {
        cohort.waiting.remove(id);
    }
    if cohort.waiting.is_empty() {
        cohort.status = CohortStatus::Released;
    }
    let done = cohort.status == CohortStatus::Released;
    for id in step {
        let Some(policy) = state.policies.get(&id) else { continue };
        ctx.work += 4;
        let product = state.catalog.product(&policy.product_hash);
        ctx.emit(Event::PolicyIssued {
            policy: policy.id.clone(),
            number: policy.number.clone(),
            group: policy.group.clone(),
            policyholder: policy.policyholder.clone(),
            product_code: policy.product_code.clone(),
            product_hash: policy.product_hash.clone(),
            inception: policy.inception,
            lives: crate::lifecycle::life_infos(policy),
            layers: crate::lifecycle::layer_infos(policy),
            members: product.map(|p| crate::lifecycle::scheme_member_covers(p, policy)).unwrap_or_default(),
        });
        ctx.emit(Event::PolicyMigrated {
            policy: policy.id.clone(),
            cohort: name.to_string(),
            cutover: policy.migration.as_ref().map_or(ctx.date, |m| m.cutover),
            periods_billed: policy.periods_billed,
            standing: policy.standing,
            outstanding_minor: policy.outstanding_minor(),
            oldest_unpaid_due: policy.oldest_unpaid_due(),
            assumptions: policy.migration.as_ref().map(|m| m.assumptions.clone()).unwrap_or_default(),
        });
    }
    if done {
        ctx.emit(Event::CohortReleased { cohort: name.to_string() });
    }
    Ok(())
}

fn abandon(state: &mut CoreState, ctx: &mut Ctx<'_>, name: &str, limit: u32) -> Result<(), Reject> {
    if limit == 0 || limit > MAX_COHORT_STEP {
        return Err(bad("apply_budget", format!("take out between 1 and {MAX_COHORT_STEP} policies a step")));
    }
    if state.migration.cohorts.get(name).is_some_and(|c| c.status == CohortStatus::Abandoned) {
        return Ok(());
    }
    let cohort = cohort_in(state, name, &[CohortStatus::Loading, CohortStatus::Abandoning])?;
    cohort.status = CohortStatus::Abandoning;
    let (c, cutover) = (cohort.currency, cohort.cutover);
    let step: Vec<PolicyId> = cohort.waiting.iter().take(limit as usize).cloned().collect();
    for id in step {
        ctx.work += 4;
        let Some(policy) = state.policies.remove(&id) else { continue };
        // Nothing has touched the policy since it was loaded, so its
        // balances are its opening entry: post them back.
        let clearing = accounts::migration_clearing(name, c);
        let mut set = PostingSet::new();
        let owed = state.ledger.balance(&accounts::premium_receivable(&id, c).id);
        let unearned = state.ledger.natural_balance(&accounts::unearned_premium(&id, c).id);
        let held = state.ledger.natural_balance(&accounts::policy_suspense(&id, c).id);
        set.credit(accounts::premium_receivable(&id, c), owed, dims(&policy, None, "premium"));
        // Unearned premium goes back under the cover types it came in on.
        for r in &policy.receivables {
            for (cover_type, net) in &r.net_fine {
                let left = net - r.earned_fine.get(cover_type).copied().unwrap_or(0);
                set.debit(accounts::unearned_premium(&id, c), left, dims(&policy, Some(cover_type), "premium"));
            }
        }
        set.debit(accounts::policy_suspense(&id, c), held, dims(&policy, None, "premium"));
        set.debit(
            clearing,
            owed - div_half_even(unearned, FINE_PER_MINOR) - held,
            dims(&policy, None, "migration"),
        );
        if !set.is_empty() {
            let mut detail = BTreeMap::new();
            detail.insert("cohort".to_string(), name.to_string());
            let postings = set.into_postings(&mut state.ledger)?;
            ctx.post(&mut state.ledger, "migration_abandoned", Some(&policy), cutover, postings, detail)?;
        }
        state.forget_holder(&policy);
        if let Some(cohort) = state.migration.cohorts.get_mut(name) {
            cohort.waiting.remove(&id);
        }
    }
    let cohort = state
        .migration
        .cohorts
        .get_mut(name)
        .ok_or_else(|| bad("unknown_cohort", "the cohort went away"))?;
    if cohort.waiting.is_empty() {
        cohort.status = CohortStatus::Abandoned;
        cohort.loaded = ControlTotals::default();
        ctx.emit(Event::CohortAbandoned { cohort: name.to_string() });
    }
    Ok(())
}
