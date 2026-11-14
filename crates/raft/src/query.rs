//! Questions a caller may ask a partition without changing it.
//!
//! Each member keeps, beside the core's state, the read models of its own
//! partition: the policy directory, the billing view and the life index.
//! They are fed from the events each applied command produces, so they are a
//! pure function of the log like everything else a member holds, and they
//! travel in the snapshot. A query is answered by the leader after it has
//! confirmed with a majority that it still leads, so an answer never
//! reflects less than what the cluster had confirmed when it was asked.

use chrono::NaiveDate;
use ic_cashmatch::{Candidates, IncomingPayment, Matcher};
use ic_core::command::IssuePolicy;
use ic_core::policy::Policy;
use ic_core::{CoreState, PolicyId, Reject};
use ic_product::rating::ModalPremium;
use ic_readmodel::{BillingRow, DirectoryEntry, Projections};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    /// The partition's business date.
    BusinessDate,
    /// A dry run of an application: the first period's bill.
    Quote { application: Box<IssuePolicy> },
    /// A policy by its number, if this partition holds it.
    PolicyByNumber { number: String },
    /// Whether this partition holds a policy.
    Locate { policy: PolicyId },
    /// What this partition's policies offer as matches for a payment.
    PaymentCandidates { matcher: Matcher, payment: IncomingPayment },
    /// A question for the claims held on this partition.
    Claims { question: ic_core::claims_module::report::Question },
    /// A question for the investment ledger on this partition.
    Investment { question: ic_core::invest::Question },
    /// Withheld tax of one kind this partition still holds, fund by fund.
    WithholdingHeld { kind: String, currency: ic_money::Currency },
    /// Every product and rate table version on this partition and whether
    /// it is in use today.
    Register,
    /// A migration cohort on this partition: its control totals, what has
    /// been loaded and where it has got to.
    Cohort { cohort: String },
    /// Underwriting loadings, a page of policies at a time. Sensitive: the
    /// caller is finance, and access is decided where the question enters.
    Loadings {
        #[serde(default)]
        after: Option<PolicyId>,
        limit: u32,
        #[serde(default)]
        as_at: Option<NaiveDate>,
    },
}

/// A policy with what the read side knows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyView {
    pub entry: DirectoryEntry,
    pub policy: Policy,
    pub billing: Option<BillingRow>,
    pub business_date: Option<NaiveDate>,
}

/// What a query is answered with. On the wire an answer is just its value:
/// the question decides the type, and [`crate::ClusterClient`] has one typed
/// method per question, so a caller cannot mistake one answer for another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Answer {
    BusinessDate(Option<NaiveDate>),
    /// `None` until the partition's business date is open.
    Quote(Option<Result<ModalPremium, Reject>>),
    Policy(Option<Box<PolicyView>>),
    Located(Option<DirectoryEntry>),
    Candidates(Candidates),
    /// `None` until the partition's business date is open.
    Claims(Option<ic_core::claims_module::report::Reply>),
    Investment(ic_core::invest::Reply),
    Loadings(ic_core::loadings::LoadingsPage),
    WithholdingHeld(Vec<ic_core::remittance::HeldByFund>),
    Cohort(Option<Box<ic_core::migration::Cohort>>),
    Register(Vec<ic_core::catalog::RegisterRow>),
}

/// Answer a query from a partition's state and read models.
pub fn answer(core: &CoreState, view: &Projections, query: Query) -> Answer {
    match query {
        Query::BusinessDate => Answer::BusinessDate(core.business_date),
        Query::Quote { application } => Answer::Quote(core.business_date.map(|today| ic_core::quote(&core.catalog, *application, today))),
        Query::PolicyByNumber { number } => Answer::Policy(view.by_number(&number).and_then(|entry| {
            let policy = core.policies.get(&entry.policy)?;
            Some(Box::new(PolicyView {
                entry: entry.clone(),
                policy: policy.clone(),
                billing: view.billing(&entry.policy).cloned(),
                business_date: core.business_date,
            }))
        })),
        Query::Locate { policy } => Answer::Located(view.policy(&policy).cloned()),
        Query::PaymentCandidates { matcher, payment } => Answer::Candidates(matcher.candidates(view, &payment)),
        Query::Claims { question } => Answer::Claims(core.business_date.map(|today| core.claims.answer(&question, today))),
        Query::Investment { question } => Answer::Investment(core.investment.answer(&core.ledger, &core.policies, &question)),
        Query::WithholdingHeld { kind, currency } => Answer::WithholdingHeld(ic_core::remittance::held(&core.withheld, &kind, currency)),
        Query::Register => Answer::Register(core.business_date.map(|today| core.catalog.register(today)).unwrap_or_default()),
        Query::Cohort { cohort } => Answer::Cohort(core.migration.cohorts.get(&cohort).cloned().map(Box::new)),
        Query::Loadings { after, limit, as_at } => Answer::Loadings(ic_core::loadings::page(&core.policies, after.as_ref(), limit, as_at)),
    }
}
