//! Issue, anniversary, cancellation, reinstatement and layer housekeeping.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate};
use ic_ledger::{Dimensions, Ledger};
use ic_money::{round_to_money, AccrualAmount, Dec, FINE_PER_MINOR};
use ic_product::schedule::{age_on, anniversary, period_start};
use ic_product::{PremiumType, ProductDefinition};

use crate::accounts::{self, dims};
use crate::bitemporal::Bitemporal;
use crate::catalog::Catalog;
use crate::command::IssuePolicy;
use crate::event::{Event, LayerInfo, LifeInfo, MemberCover};
use crate::machine::{CoreState, Ctx, Reject, MAX_MEMBERSHIP_CHUNK};
use crate::policy::{CancellationRecord, Cover, CoverStatus, Layer, LayerOrigin, LifeFactors, LifeKind, LifeRecord, Policy, PolicyStatus, Standing};
use crate::postings::{div_half_even, PostingSet};
use crate::premium::{member_period_premium, member_sum_insured, retail_period_premium};
use crate::{arrears, billing, cash, earning, LifeId};

/// How far back a policy may be incepted or a change backdated.
pub(crate) const BACKDATING_LIMIT_DAYS: u64 = 400;

pub(crate) fn life_infos(policy: &Policy) -> Vec<LifeInfo> {
    policy
        .lives
        .values()
        .map(|l| {
            let party = match &l.kind {
                LifeKind::Named { party } => Some(party.clone()),
                LifeKind::UnnamedMember { linked_party, .. } => linked_party.clone(),
                LifeKind::Pool { .. } => None,
            };
            LifeInfo::with_factors(l.id.clone(), party, l.factors.as_of(NaiveDate::MAX))
        })
        .collect()
}

pub(crate) fn layer_infos(policy: &Policy) -> Vec<LayerInfo> {
    policy
        .covers
        .values()
        .flat_map(|c| {
            c.layers.iter().filter(|l| l.end.is_none()).map(move |l| LayerInfo {
                cover: c.code.clone(),
                cover_type: c.cover_type.clone(),
                layer: l.id,
                life: l.life.clone(),
                sum_insured_minor: l.sum_insured_minor,
                start: l.start,
            })
        })
        .collect()
}

/// A scheme member's cover under each formula-driven cover, on the given
/// details.
pub(crate) fn member_covers(product: &ProductDefinition, policy: &Policy, life: &LifeId, factors: &LifeFactors) -> Vec<MemberCover> {
    policy
        .covers
        .values()
        .filter(|c| c.formula_driven && c.status == CoverStatus::InForce)
        .filter_map(|c| {
            let def = product.cover(&c.code)?;
            let sum_insured_minor = member_sum_insured(def, factors, life, product.rounding, product.currency).ok()?;
            Some(MemberCover {
                cover: c.code.clone(),
                cover_type: c.cover_type.clone(),
                life: life.clone(),
                sum_insured_minor,
            })
        })
        .collect()
}

/// Formula-driven cover on every current member of a scheme, on each
/// member's latest notified details. Empty for a retail policy.
pub(crate) fn scheme_member_covers(product: &ProductDefinition, policy: &Policy) -> Vec<MemberCover> {
    policy
        .lives
        .values()
        .filter(|l| l.left.is_none() && !matches!(l.kind, LifeKind::Pool { .. }))
        .filter_map(|l| Some(member_covers(product, policy, &l.id, l.factors.as_of(NaiveDate::MAX)?)))
        .flatten()
        .collect()
}

/// Build a policy from an application, validating it against the product.
/// Shared by issue and by quotes, so the two can never disagree.
pub(crate) fn build_policy(catalog: &Catalog, cmd: IssuePolicy, today: NaiveDate, index: u64) -> Result<(Policy, ProductDefinition), Reject> {
    if cmd.inception > today
        || today
            .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
            .is_some_and(|limit| cmd.inception < limit)
    {
        return Err(Reject::new(
            "inception",
            "inception must be on or before the business date and inside the backdating limit",
        ));
    }
    let product = catalog
        .product_on(&cmd.product_code, cmd.inception)
        .ok_or_else(|| Reject::new("unknown_product", format!("no version of {} is active for {}", cmd.product_code, cmd.inception)))?
        .clone();
    if !product.allows_frequency(cmd.frequency) {
        return Err(Reject::new("frequency", "the product does not offer that premium frequency"));
    }
    if cmd.lives.is_empty() || cmd.covers.is_empty() {
        return Err(Reject::new("invalid_policy", "a policy needs at least one life and one cover"));
    }
    if cmd.lives.len() > MAX_MEMBERSHIP_CHUNK {
        return Err(Reject::new(
            "apply_budget",
            format!("issue with at most {MAX_MEMBERSHIP_CHUNK} lives, then add members in chunks"),
        ));
    }
    let mut lives = BTreeMap::new();
    for l in cmd.lives {
        let rec = LifeRecord {
            id: l.id.clone(),
            kind: l.kind,
            factors: Bitemporal::new(cmd.inception, today, index, l.factors),
            joined: cmd.inception,
            left: None,
        };
        if lives.insert(l.id.clone(), rec).is_some() {
            return Err(Reject::new("invalid_policy", format!("life {} is listed twice", l.id)));
        }
    }
    let mut covers: BTreeMap<String, Cover> = BTreeMap::new();
    for spec in cmd.covers {
        let def = product
            .cover(&spec.code)
            .ok_or_else(|| Reject::new("unknown_cover", format!("the product does not offer cover {}", spec.code)))?;
        let mut bindings = Vec::new();
        for id in &def.definitions {
            bindings.push(
                catalog
                    .bind_definition(id, cmd.inception)
                    .ok_or_else(|| Reject::new("unknown_definition", format!("definition {id} has no version in force on {}", cmd.inception)))?,
            );
        }
        let formula = def.rating.premium_type == PremiumType::GroupUnitRate;
        let cover = covers.entry(spec.code.clone()).or_insert_with(|| Cover {
            code: def.code.clone(),
            cover_type: def.cover_type.clone(),
            status: CoverStatus::InForce,
            start: cmd.inception,
            definitions: bindings,
            layers: vec![],
            formula_driven: formula,
        });
        if !formula {
            let life = spec
                .life
                .ok_or_else(|| Reject::new("invalid_policy", format!("cover {} needs a life insured", spec.code)))?;
            if !lives.contains_key(&life) {
                return Err(Reject::new("invalid_policy", format!("life {life} is not on the policy")));
            }
            let total: i64 = cover.layers.iter().filter(|l| l.life == life).map(|l| l.sum_insured_minor).sum::<i64>() + spec.sum_insured_minor;
            ic_product::rating::check_sum_insured(def, total).map_err(|e| Reject::new("sum_insured", format!("cover {}: {e}", spec.code)))?;
            let id = cover.layers.len() as u32 + 1;
            cover.layers.push(Layer {
                id,
                life,
                start: cmd.inception,
                end: None,
                sum_insured_minor: spec.sum_insured_minor,
                origin: LayerOrigin::Original,
                loadings: spec.loadings,
                exclusions: spec.exclusions,
                loading_itemised: false,
                rated_from: None,
            });
        }
    }
    let policy = Policy {
        id: cmd.policy.clone(),
        number: cmd.number,
        group: cmd.group,
        policyholder: cmd.policyholder,
        product_code: product.code.clone(),
        product_hash: product.content_hash.clone(),
        currency: product.currency,
        statutory_fund: product.statutory_fund.clone(),
        inception: cmd.inception,
        frequency: cmd.frequency,
        status: PolicyStatus::InForce,
        standing: Some(Standing::Paid),
        lives,
        covers,
        indexation_declined: false,
        reopen_windows: BTreeMap::new(),
        notices: BTreeMap::new(),
        cancellation: None,
        migrated: cmd.migrated,
        declined_anniversaries: Vec::new(),
        periods_billed: 0,
        next_receivable_id: 1,
        receivables: vec![],
        receipts: BTreeMap::new(),
        anniversaries_processed: 0,
        pending_invoice: None,
        lapse: None,
        transfer_out: None,
        history: vec![],
        unwind: None,
        refund_with_claim: None,
        claims: BTreeMap::new(),
        refunds_raised: BTreeMap::new(),
        payouts: BTreeMap::new(),
        interests: BTreeMap::new(),
        payments: BTreeMap::new(),
        cessions: BTreeMap::new(),
    };
    Ok((policy, product))
}

/// A dry run: the premium a policy would be billed for its first period.
/// Never goes through the log.
pub fn quote(catalog: &Catalog, application: IssuePolicy, today: NaiveDate) -> Result<ic_product::rating::ModalPremium, Reject> {
    let (policy, product) = build_policy(catalog, application, today, 0)?;
    let mut work = 0;
    let mut premium = retail_period_premium(catalog, &product, &policy, policy.inception, &mut work)?;
    if policy.covers.values().any(|c| c.formula_driven) {
        for life in policy.lives.values() {
            crate::premium::add_premium(&mut premium, &member_period_premium(catalog, &product, &policy, life, policy.inception)?)?;
        }
    }
    Ok(premium)
}

pub(crate) fn issue(state: &mut CoreState, ctx: &mut Ctx<'_>, cmd: IssuePolicy) -> Result<(), Reject> {
    if state.policies.contains_key(&cmd.policy) {
        return Err(Reject::new("policy_exists", format!("policy {} already exists", cmd.policy)));
    }
    if cmd.migrated != (ctx.caller == "migration") {
        return Err(Reject::new(
            "migrated",
            "a migrated policy is issued by the migration load, and the migration load issues nothing else",
        ));
    }
    let (mut policy, product) = build_policy(&state.catalog, cmd, ctx.date, ctx.index)?;
    // Rate everything before anything is written, so a policy that cannot be
    // priced is refused whole.
    retail_period_premium(&state.catalog, &product, &policy, policy.inception, &mut ctx.work)?;
    if policy.covers.values().any(|c| c.formula_driven) {
        for life in policy.lives.values() {
            ctx.work += 2;
            member_period_premium(&state.catalog, &product, &policy, life, policy.inception)?;
        }
    }
    for a in accounts::policy_accounts(&policy.id, policy.currency) {
        state.ledger.open_account(a)?;
    }
    ctx.emit(Event::PolicyIssued {
        policy: policy.id.clone(),
        number: policy.number.clone(),
        group: policy.group.clone(),
        policyholder: policy.policyholder.clone(),
        product_code: policy.product_code.clone(),
        product_hash: policy.product_hash.clone(),
        inception: policy.inception,
        lives: life_infos(&policy),
        layers: layer_infos(&policy),
        members: scheme_member_covers(&product, &policy),
    });
    // Premium is in advance: the first period is due at issue.
    let r = billing::bill_due_periods(&mut state.ledger, &state.catalog, &product, &mut policy, ctx);
    policy.stamp_settled(ctx.date);
    state.policies.insert(policy.id.clone(), policy);
    r.map(|_| ())
}

/// Process every anniversary that has arrived: cover expiry by age, then
/// indexation (automatic unless declined). Stepped premiums re-rate on their
/// own because billing rates as at the last anniversary.
pub(crate) fn anniversaries(catalog: &Catalog, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    loop {
        let n = policy.anniversaries_processed + 1;
        let Some(on) = anniversary(policy.inception, n) else {
            break;
        };
        if on > ctx.date {
            break;
        }
        ctx.emit(Event::AnniversaryReached {
            policy: policy.id.clone(),
            number: n,
            on,
        });
        let lives = &policy.lives;
        let mut expired = Vec::new();
        for cover in policy.covers.values_mut().filter(|c| c.status == CoverStatus::InForce && !c.formula_driven) {
            let Some(def) = product.cover(&cover.code) else {
                continue;
            };
            let Some(limit) = def.expiry_age else {
                continue;
            };
            for layer in cover.layers.iter_mut().filter(|l| l.end.is_none()) {
                ctx.work += 1;
                let dob = lives.get(&layer.life).and_then(|l| l.factors.as_of(on)).and_then(|f| f.date_of_birth);
                if dob.is_some_and(|d| age_on(d, on) >= limit) {
                    layer.end = Some(on);
                }
            }
            if cover.layers.iter().all(|l| l.end.is_some()) {
                cover.status = CoverStatus::Expired;
                expired.push(cover.code.clone());
            }
        }
        for code in expired {
            ctx.emit(Event::CoverExpired {
                policy: policy.id.clone(),
                cover: code,
                on,
            });
        }
        if product.indexation().is_some() && policy.indexation_declined {
            policy.declined_anniversaries.push(n);
        } else {
            apply_indexation(catalog, product, policy, ctx, on)?;
        }
        policy.anniversaries_processed = n;
        if policy.covers.values().all(|c| c.status != CoverStatus::InForce) {
            arrears::set_standing(policy, ctx, None);
            ctx.emit(Event::PolicyStatusChanged {
                policy: policy.id.clone(),
                from: policy.status,
                to: PolicyStatus::Expired,
            });
            policy.status = PolicyStatus::Expired;
            break;
        }
    }
    Ok(())
}

/// Add the indexation step of the anniversary on `on` to every cover in
/// force, as a new layer per life that starts that day.
pub(crate) fn apply_indexation(catalog: &Catalog, product: &ProductDefinition, policy: &mut Policy, ctx: &mut Ctx<'_>, on: NaiveDate) -> Result<(), Reject> {
    if product.indexation().is_some() {
        if let Some(code) = product.indexation() {
            if let Some(rate) = catalog.index_rate_on(code, on) {
                let mut added = Vec::new();
                for cover in policy.covers.values_mut().filter(|c| c.status == CoverStatus::InForce && !c.formula_driven) {
                    let Some(def) = product.cover(&cover.code) else {
                        continue;
                    };
                    let lives_on_cover: BTreeSet<_> = cover.layers.iter().filter(|l| l.in_force_on(on)).map(|l| l.life.clone()).collect();
                    for life in lives_on_cover {
                        ctx.work += 1;
                        let current: i64 = cover
                            .layers
                            .iter()
                            .filter(|l| l.life == life && l.in_force_on(on))
                            .map(|l| l.sum_insured_minor)
                            .sum();
                        let mut step = round_to_money(Dec::from(current) / Dec::from(100i64) * rate.get(), policy.currency, product.rounding)?.minor;
                        if let Some(max) = def.max_sum_insured_minor {
                            step = step.min(max - current);
                        }
                        if step <= 0 {
                            continue;
                        }
                        // The step carries the same underwriting terms as the cover it indexes.
                        let (loadings, exclusions) = cover
                            .layers
                            .iter()
                            .find(|l| l.life == life && l.in_force_on(on))
                            .map(|l| (l.loadings.clone(), l.exclusions.clone()))
                            .unwrap_or_default();
                        let id = cover.layers.iter().map(|l| l.id).max().unwrap_or(0) + 1;
                        cover.layers.push(Layer {
                            id,
                            life: life.clone(),
                            start: on,
                            end: None,
                            sum_insured_minor: step,
                            origin: LayerOrigin::Indexation,
                            loadings,
                            exclusions,
                            loading_itemised: false,
                            rated_from: None,
                        });
                        added.push(LayerInfo {
                            cover: cover.code.clone(),
                            cover_type: cover.cover_type.clone(),
                            layer: id,
                            life,
                            sum_insured_minor: step,
                            start: on,
                        });
                    }
                }
                for layer in added {
                    ctx.emit(Event::LayerAdded {
                        policy: policy.id.clone(),
                        layer,
                        origin: LayerOrigin::Indexation,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Combine layers that nothing distinguishes, so decades of indexation do
/// not leave hundreds of layers. Only layers older than the backdating window
/// are merged, so nothing that could still be recalculated changes.
///
/// A level premium is priced on the life's age when the layer was first
/// rated, so on a level cover layers also need the same rating date to merge;
/// otherwise the merge would reprice the later layer at the earlier age.
pub(crate) fn merge_layers(catalog: &Catalog, policy: &mut Policy, cover_code: &str, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let level = catalog
        .product(&policy.product_hash)
        .and_then(|p| p.cover(cover_code))
        .is_some_and(|def| def.rating.premium_type == PremiumType::Level);
    let cutoff = ctx
        .date
        .checked_sub_days(Days::new(BACKDATING_LIMIT_DAYS))
        .ok_or_else(|| Reject::new("date", "date out of range"))?;
    let id = policy.id.clone();
    let cover = policy
        .covers
        .get_mut(cover_code)
        .ok_or_else(|| Reject::new("unknown_cover", "no such cover on the policy"))?;
    let mut merged_any = false;
    let mut i = 0;
    while i < cover.layers.len() {
        if cover.layers[i].start > cutoff || cover.layers[i].end.is_some() {
            i += 1;
            continue;
        }
        let mut removed = Vec::new();
        let mut j = i + 1;
        while j < cover.layers.len() {
            if cover.layers[j].start <= cutoff
                && cover.layers[i].same_terms(&cover.layers[j])
                && (!level || cover.layers[i].rating_start() == cover.layers[j].rating_start())
            {
                let l = cover.layers.remove(j);
                cover.layers[i].sum_insured_minor += l.sum_insured_minor;
                removed.push(l.id);
            } else {
                j += 1;
            }
        }
        if !removed.is_empty() {
            merged_any = true;
            ctx.emit(Event::LayersMerged {
                policy: id.clone(),
                cover: cover_code.to_string(),
                removed,
                into: cover.layers[i].id,
                sum_insured_minor: cover.layers[i].sum_insured_minor,
            });
        }
        i += 1;
    }
    if merged_any {
        Ok(())
    } else {
        Err(Reject::new("nothing_to_merge", "no layers on this cover can be merged"))
    }
}

/// Cancellation. Inside the cooling-off period all premium is refunded and
/// the receivables are reversed in full. After it, cover ends today and the
/// unearned premium for the rest of each billed period is returned, with the
/// tax that went with it. Refunds land in policy suspense and are paid out by
/// the payments module.
pub(crate) fn cancel(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    if policy.status != PolicyStatus::InForce {
        return Err(Reject::new("not_in_force", "only a policy in force can be cancelled"));
    }
    if policy.pending_invoice.is_some() {
        return Err(Reject::new("invoice_in_progress", "a scheme invoice is being built; retry shortly"));
    }
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let cooling_off = product.cooling_off_days().is_some_and(|d| (ctx.date - policy.inception).num_days() <= d as i64);
    if !cooling_off {
        if let Some(yesterday) = ctx.date.pred_opt() {
            earning::earn_through(ledger, policy, ctx, yesterday)?;
        }
    }
    let kind = if cooling_off { "cancelled_cooling_off" } else { "cancelled" };
    let mut record = CancellationRecord {
        cancelled_on: ctx.date,
        entry: None,
        postings: Vec::new(),
        receivables_before: policy.receivables.clone(),
        covers_before: policy.covers.clone(),
        standing_before: policy.standing,
    };
    let posted = ctx.entries.len();
    close_out(ledger, &product, policy, ctx, cooling_off, kind, ctx.date, ctx.date, BTreeMap::new())?;
    if let Some(entry) = ctx.entries.get(posted) {
        record.entry = Some(entry.id);
        record.postings = entry.body.postings.clone();
    }
    policy.cancellation = Some(record);
    arrears::set_standing(policy, ctx, None);
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::Cancelled,
    });
    policy.status = PolicyStatus::Cancelled;
    Ok(())
}

/// Close every open receivable and end every cover: the unearned premium
/// and the tax that went with it are returned to policy suspense (in the
/// cooling-off period, the earned premium too), and premium still owed for
/// cover already given is written off. The caller has earned the policy up
/// to the last day of cover and sets the status afterwards.
#[allow(clippy::too_many_arguments)]
pub(crate) fn close_out(
    ledger: &mut Ledger,
    product: &ProductDefinition,
    policy: &mut Policy,
    ctx: &mut Ctx<'_>,
    cooling_off: bool,
    entry_kind: &str,
    value_date: NaiveDate,
    cover_ends: NaiveDate,
    detail: BTreeMap<String, String>,
) -> Result<(), Reject> {
    let c = policy.currency;
    let mut set = PostingSet::new();
    let mut refund_fine: i128 = 0;
    for r in policy.receivables.iter().filter(|r| !r.closed) {
        let net = r.net_total_fine();
        let unearned = r.unearned_fine();
        set.debit(accounts::unearned_premium(&policy.id, c), unearned, dims(policy, None, "cancellation"));
        let mut returned = unearned;
        if cooling_off {
            for (cover_type, earned) in &r.earned_fine {
                set.debit(
                    accounts::earned_premium(&policy.product_code, cover_type, c),
                    *earned,
                    dims(policy, Some(cover_type), "cancellation"),
                );
                returned += earned;
            }
        }
        // Tax and levies follow the premium that is returned.
        let mut credit_minor: i128 = 0;
        if net != 0 {
            let gst = div_half_even(r.gst_minor as i128 * returned, net);
            set.debit(accounts::gst_payable(c), gst, dims(policy, None, "gst"));
            credit_minor += gst;
            for (code, v) in &r.levies_minor {
                let l = div_half_even(*v as i128 * returned, net);
                set.debit(accounts::levy_payable(code, c), l, dims(policy, None, "levy"));
                credit_minor += l;
            }
        }
        // First against what is still owed on this receivable, the rest back
        // to the policyholder. Premium owed for cover already given is
        // written off: nothing is chased after a cancellation.
        let outstanding = r.outstanding_minor.max(0) as i128;
        let returned_minor_floor = returned.div_euclid(FINE_PER_MINOR);
        let against = outstanding.min(returned_minor_floor + credit_minor);
        set.credit(accounts::premium_receivable(&policy.id, c), outstanding, dims(policy, None, "cancellation"));
        set.debit(
            accounts::lapse_write_off(&policy.product_code, c),
            (outstanding - against) * FINE_PER_MINOR,
            dims(policy, None, "cancellation"),
        );
        refund_fine += returned + (credit_minor - against) * FINE_PER_MINOR;
    }
    let (refund, rem) = AccrualAmount::new(refund_fine, c).round_to_money(product.rounding)?;
    set.credit(accounts::policy_suspense(&policy.id, c), refund.minor as i128, Dimensions::default());
    set.credit(accounts::rounding(c), rem.fine, dims(policy, None, "rounding"));
    let postings = set.into_postings(ledger)?;
    if !postings.is_empty() {
        ctx.post(ledger, entry_kind, Some(policy), value_date, postings, detail)?;
    }
    for r in policy.receivables.iter_mut() {
        r.outstanding_minor = 0;
        r.closed = true;
    }
    for cover in policy.covers.values_mut() {
        cover.status = CoverStatus::Ended;
        for l in cover.layers.iter_mut().filter(|l| l.end.is_none()) {
            // A layer cannot end before it starts.
            l.end = Some(cover_ends.max(l.start));
        }
    }
    Ok(())
}

/// Reinstatement inside the product's window. The arrears must already be in
/// policy suspense. The lapse entry is reversed exactly, the missed periods
/// are billed, the money is applied and the earned premium for the gap is
/// recognised at this point.
pub(crate) fn reinstate(ledger: &mut Ledger, catalog: &Catalog, policy: &mut Policy, ctx: &mut Ctx<'_>) -> Result<(), Reject> {
    let Some(lapse) = policy.lapse.clone().filter(|_| policy.status == PolicyStatus::Lapsed) else {
        return Err(Reject::new("not_lapsed", "only a lapsed policy can be reinstated"));
    };
    let product = catalog
        .product(&policy.product_hash)
        .ok_or_else(|| Reject::new("unknown_product", "product version not active"))?
        .clone();
    let window = product.arrears().map_or(0, |r| r.reinstatement_days) as i64;
    if (ctx.date - lapse.lapsed_on).num_days() > window {
        return Err(Reject::new("window_closed", "the reinstatement window has closed; this needs a new policy"));
    }
    // What the missed periods will cost, priced before anything is written.
    let mut missed: i64 = 0;
    if !policy.covers.values().any(|c| c.formula_driven) {
        let mut n = policy.periods_billed;
        while let Some(start) = period_start(policy.inception, policy.frequency, n) {
            if start > ctx.date {
                break;
            }
            missed += retail_period_premium(catalog, &product, policy, start, &mut ctx.work)?.total();
            n += 1;
        }
    }
    let c = policy.currency;
    let required = lapse.arrears_minor + missed;
    let held = ledger.available(&accounts::policy_suspense(&policy.id, c).id);
    if held < required as i128 {
        return Err(Reject::new(
            "arrears_unpaid",
            format!("reinstatement needs {required} in suspense; the policy holds {held}"),
        ));
    }
    if !lapse.postings.is_empty() {
        let reversal = lapse.postings.iter().map(ic_ledger::Posting::reversed).collect();
        ctx.post(ledger, "lapse_reversed", Some(policy), ctx.date, reversal, BTreeMap::new())?;
    }
    policy.receivables = lapse.receivables_before;
    policy.lapse = None;
    ctx.emit(Event::PolicyStatusChanged {
        policy: policy.id.clone(),
        from: policy.status,
        to: PolicyStatus::InForce,
    });
    policy.status = PolicyStatus::InForce;
    ctx.emit(Event::PolicyReinstated { policy: policy.id.clone() });
    cash::apply_suspense(ledger, policy, ctx)?;
    while period_start(policy.inception, policy.frequency, policy.periods_billed).is_some_and(|s| s <= ctx.date) {
        if !billing::bill_due_periods(ledger, catalog, &product, policy, ctx)? {
            break;
        }
    }
    if let Some(yesterday) = ctx.date.pred_opt() {
        earning::earn_through(ledger, policy, ctx, yesterday)?;
    }
    arrears::refresh(&product, policy, ctx);
    Ok(())
}
