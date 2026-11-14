//! New business attributed to the adviser who wrote it.
//!
//! New business knows the adviser, the provider the adviser acted under,
//! the commission option chosen and each cover's annual premium. It does not
//! know this module's engagements, books or schedules, so it sends
//! `AdviseNewBusiness` and the module writes each of the policy's layers
//! itself: the engagement in force between that adviser and that provider
//! on the layer's start, the book that engagement owns, and the schedule the
//! configuration names for the product (and cover type). The command may
//! arrive before or after the core's `PolicyIssued`: before, it waits for
//! the policy; after, the layers are written at once. Anything that cannot
//! be resolved raises a task for commission operations instead of failing
//! silently, and a layer already written is left as it is.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId, Reject};
use serde::{Deserialize, Serialize};

use crate::rules::WHOLE;
use crate::state::{reject, Ctx, DistributionState, WriteLayer};
use crate::{BookId, EngagementId};

/// What new business says about who wrote a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewBusiness {
    pub adviser: PartyId,
    /// The financial advice provider (or broker) the adviser acted under.
    pub provider: PartyId,
    /// The commission option the adviser chose, for example `upfront`.
    pub option: String,
    /// The application replaces cover the customer holds: trail only.
    #[serde(default)]
    pub replacement: bool,
    /// Each cover's annual premium, by cover code.
    pub premiums: BTreeMap<String, i64>,
}

/// The configuration key for a product's schedule, optionally narrowed to
/// one cover type.
pub fn schedule_key(product: &str, cover_type: Option<&str>) -> String {
    match cover_type {
        Some(t) => format!("{product}/{t}"),
        None => product.to_string(),
    }
}

impl DistributionState {
    pub(crate) fn advise_new_business(&mut self, ctx: &mut Ctx, policy: PolicyId, advice: NewBusiness) -> Result<(), Reject> {
        if advice.option.trim().is_empty() || advice.premiums.is_empty() || advice.premiums.values().any(|p| *p <= 0) {
            return reject(
                "invalid_new_business",
                "new business names the commission option and a positive annual premium for each cover",
            );
        }
        if self.new_business.contains_key(&policy) || self.policy_written(&policy) {
            return reject("already_advised", format!("policy {policy} has already been attributed to an adviser"));
        }
        self.new_business.insert(policy.clone(), advice);
        if self.policies.contains_key(&policy) {
            self.attribute_new_business(ctx, &policy);
        }
        Ok(())
    }

    fn policy_written(&self, policy: &PolicyId) -> bool {
        self.policies
            .get(policy)
            .is_some_and(|p| p.layers.iter().filter_map(|k| self.layers.get(k)).any(|l| !l.writing.is_empty()))
    }

    /// The policy is known: write each of its layers that nobody has
    /// written, from what new business said. Called when the advice arrives
    /// after the policy, and when the policy is issued after the advice.
    pub(crate) fn attribute_new_business(&mut self, ctx: &mut Ctx, policy: &PolicyId) {
        let Some(advice) = self.new_business.remove(policy) else { return };
        let Some(record) = self.policies.get(policy).cloned() else { return };
        let due = self.today.unwrap_or(record.inception);
        let mut problems: Vec<String> = vec![];
        for key in &record.layers {
            let Some(layer) = self.layers.get(key).cloned() else { continue };
            if !layer.writing.is_empty() || layer.terms.is_some() || layer.indexation {
                continue;
            }
            let found = self.resolve(&advice, &record.product_code, &layer.cover, &layer.cover_type, layer.start);
            let written = found.and_then(|(engagement, book, schedule, premium)| {
                self.write_layer(
                    ctx,
                    WriteLayer {
                        policy: policy.clone(),
                        cover: layer.cover.clone(),
                        layer: layer.layer,
                        writing: vec![(engagement, WHOLE)],
                        books: vec![(book, WHOLE)],
                        schedule,
                        option: advice.option.clone(),
                        annual_premium_minor: premium,
                        replacement: advice.replacement,
                    },
                )
            });
            if let Err(r) = written {
                problems.push(format!("{key}: {} ({})", r.message, r.code));
            }
        }
        if !problems.is_empty() {
            self.task(ctx, "new_business_unattributed", &policy.0, due, problems.join("; "));
        }
    }

    /// The engagement, book, schedule and premium for one layer.
    fn resolve(
        &self,
        advice: &NewBusiness,
        product: &str,
        cover: &str,
        cover_type: &str,
        start: NaiveDate,
    ) -> Result<(EngagementId, BookId, String, i64), Reject> {
        let structure = &self.config.structure;
        let engagement = structure
            .engagements
            .iter()
            .find(|(_, e)| e.adviser == advice.adviser && e.with == advice.provider && e.on(start))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| {
                Reject::new(
                    "no_engagement",
                    format!("{} has no engagement with {} in force on {start}", advice.adviser, advice.provider),
                )
            })?;
        let book = self
            .config
            .books
            .iter()
            .find(|(_, b)| b.engagement.as_ref() == Some(&engagement) && b.closed_to_new_from.is_none_or(|c| start < c))
            .map(|(id, _)| id.clone())
            .ok_or_else(|| Reject::new("no_book", format!("engagement {engagement} has no book open to new business")))?;
        let schedules = &self.config.product_schedules;
        let schedule = schedules
            .get(&schedule_key(product, Some(cover_type)))
            .or_else(|| schedules.get(&schedule_key(product, None)))
            .cloned()
            .ok_or_else(|| Reject::new("no_schedule", format!("no commission schedule is named for {product}")))?;
        let premium = advice
            .premiums
            .get(cover)
            .copied()
            .ok_or_else(|| Reject::new("no_premium", format!("new business gave no premium for cover {cover}")))?;
        Ok((engagement, book, schedule, premium))
    }
}
