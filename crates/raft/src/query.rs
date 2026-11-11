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
    }
}
