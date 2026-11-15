//! Versioned configuration: who may decide how much, and the rules claims
//! are triaged, timed and checked by. Both are activated through the log
//! with a preparer and approvers, and every decision records the versions it
//! was made under.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{ClaimType, DecisionKind};
use crate::port::Reject;
use crate::StaffId;

/// A line of business an assessor is accredited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Line {
    Health,
    LumpSum,
    IncomeProtection,
}

/// Work queues, by skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Queue {
    Health,
    LumpSum,
    IncomeProtectionNew,
    IncomeProtectionOngoing,
    Group,
    /// The senior queue: large, complex and reversal-of-admission work.
    ComplexLarge,
    MedicalOfficer,
    Rehabilitation,
    Recoveries,
    QualityReview,
}

/// What kind of action a person may take. Small, and they combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Assessor,
    QueueLead,
    ClaimsManager,
    MedicalOfficer,
    QualityReviewer,
    ConductOfficer,
    Legal,
    /// Works restricted investigations, which nobody else sees.
    Investigator,
}

/// How much a person may decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Profile {
    pub lines: BTreeSet<Line>,
    /// Largest admission, by claim type. A type not listed may not be admitted.
    #[serde(default)]
    pub max_admission_minor: BTreeMap<ClaimType, i64>,
    /// Largest single payment released.
    #[serde(default)]
    pub max_payment_minor: i64,
    /// Largest reserve set without a senior.
    #[serde(default)]
    pub max_reserve_minor: i64,
    #[serde(default)]
    pub may_decline: bool,
    #[serde(default)]
    pub max_ex_gratia_minor: i64,
    #[serde(default)]
    pub max_write_off_minor: i64,
    /// May admit over an exclusion the rules raised, with a reason code.
    #[serde(default)]
    pub may_override_exclusion: bool,
    /// Reversal of admission is its own authority, above a decline.
    #[serde(default)]
    pub may_reverse_admission: bool,
    /// Changing the recovery status of an overpayment is its own authority.
    #[serde(default)]
    pub may_change_recovery_status: bool,
    /// Works the senior queue.
    #[serde(default)]
    pub senior: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Staff {
    pub profile: String,
    pub roles: BTreeSet<Role>,
    #[serde(default)]
    pub queues: BTreeSet<Queue>,
    /// Open claims this person may own at once.
    #[serde(default)]
    pub wip_limit: u32,
    #[serde(default)]
    pub vulnerable_trained: bool,
    /// Parties this person has declared a relationship with.
    #[serde(default)]
    pub related_parties: BTreeSet<String>,
}

/// Authority profiles and the staff who hold them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AuthorityBook {
    pub version: u32,
    pub profiles: BTreeMap<String, Profile>,
    pub staff: BTreeMap<StaffId, Staff>,
    pub prepared_by: StaffId,
    /// The manager and the function head.
    pub approved_by: Vec<StaffId>,
}

/// Days allowed for each service step on a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerTargets {
    pub acknowledge_days: u32,
    pub first_contact_days: u32,
    pub evidence_days: u32,
    pub decision_days: u32,
    pub payment_days: u32,
}

impl Default for TimerTargets {
    fn default() -> Self {
        Self {
            acknowledge_days: 2,
            first_contact_days: 5,
            evidence_days: 20,
            decision_days: 20,
            payment_days: 5,
        }
    }
}

/// One exclusion as the claims rules see it. The wording stays with underwriting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Exclusion {
    /// Cover types the exclusion applies to. Empty means every cover type.
    #[serde(default)]
    pub applies_to: BTreeSet<String>,
    /// Dotted cause tags. A claim tag matches a tag here and its descendants.
    #[serde(default)]
    pub cause_tags: Vec<String>,
    /// A migrated clause in words: never matched by rule, always to an assessor.
    #[serde(default)]
    pub verbatim: bool,
}

/// What the rules make of one exclusion against a claim's cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    DoesNotApply,
    NeedsAssessor,
    Applies,
}

/// The roots of the cause tag tree. A tag outside them is not recognised.
pub const CAUSE_ROOTS: &[&str] = &[
    "musculoskeletal",
    "mental",
    "cardiovascular",
    "cancer",
    "neurological",
    "respiratory",
    "digestive",
    "endocrine",
    "pregnancy",
    "infection",
    "injury",
    "pursuit",
    "occupation",
    "travel",
    "pre_existing",
    "unknown",
];

/// Limits for one health benefit category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HealthBenefit {
    pub per_event_minor: i64,
    pub per_year_minor: i64,
    pub lifetime_minor: i64,
    /// The member's share, in whole percent.
    #[serde(default)]
    pub copay_pct: u32,
    #[serde(default)]
    pub excess_minor: i64,
}

/// Simple interest on a lump sum, from the end of the free days after the
/// admission to the day the payment is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LateInterest {
    pub annual_rate_bps: u32,
    /// Days after the admission in which a payment carries no interest.
    pub free_days: u32,
}

impl LateInterest {
    /// Interest on `amount_minor` admitted on `admitted` and paid on `paid`,
    /// by days over a 365 day year, rounded down.
    pub fn on(&self, amount_minor: i64, admitted: chrono::NaiveDate, paid: chrono::NaiveDate) -> i64 {
        let days = (paid - admitted).num_days() - i64::from(self.free_days);
        if days <= 0 || amount_minor <= 0 {
            return 0;
        }
        let interest = i128::from(amount_minor) * i128::from(self.annual_rate_bps) * i128::from(days) / (10_000 * 365);
        i64::try_from(interest).unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Rules {
    pub version: u32,
    #[serde(default)]
    pub timers: BTreeMap<Line, TimerTargets>,
    /// Days after a missed target at which the owner, the queue lead and the
    /// claims manager are each told.
    #[serde(default)]
    pub escalation_days: [u32; 3],
    /// Largest amount the rules may admit without a person, by claim type.
    #[serde(default)]
    pub straight_through_max_minor: BTreeMap<ClaimType, i64>,
    /// At or above this, a claim goes to the senior queue.
    pub large_claim_minor: i64,
    /// At or above this, an admission needs two signatures.
    pub two_signature_admission_minor: i64,
    pub two_signature_ex_gratia_minor: i64,
    pub two_signature_write_off_minor: i64,
    /// The first reserve, as a percentage of the sum insured or amount claimed.
    #[serde(default)]
    pub initial_reserve_pct: BTreeMap<ClaimType, u32>,
    /// A reserve moving by at least this percentage goes to senior review.
    pub sharp_reserve_move_pct: u32,
    /// The controlled list of reason codes and the decisions each may explain.
    #[serde(default)]
    pub reason_codes: BTreeMap<String, BTreeSet<DecisionKind>>,
    /// Cause codes and the cause tags each maps to.
    #[serde(default)]
    pub cause_codes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub exclusions: BTreeMap<String, Exclusion>,
    pub medical_review_months: u32,
    pub income_declaration_months: u32,
    /// Days after a review falls due before it is flagged overdue.
    pub review_grace_days: u32,
    /// The largest share of a benefit that may be offset against an overpayment.
    pub max_offset_pct: u32,
    /// How many partitions the deployment has. An overpayment being offset
    /// is shared with every other partition, so the same person's benefits
    /// there can recover it. Zero or one: there is no other partition.
    #[serde(default)]
    pub partitions: u16,
    /// Percentage of decisions of each kind sampled for peer review.
    #[serde(default)]
    pub sample_pct: BTreeMap<DecisionKind, u32>,
    /// A death notified more than this many days after it happened prompts a
    /// check of the death register feed.
    pub late_death_days: u32,
    /// An event this many days or fewer after the cover started opens a
    /// restricted investigation (rule `early_claim`) before anything is
    /// decided. Zero: the rule is off.
    #[serde(default)]
    pub early_claim_days: u32,
    /// The most that may be advanced on a death claim before it is decided,
    /// for the funeral or the family's immediate needs. Zero: no advances.
    #[serde(default)]
    pub funeral_advance_max_minor: i64,
    /// Days added to each deadline the claimant has to meet (evidence asked
    /// of them, the grace on a review) while they carry a communication
    /// marker. It is the whole extension: the marker never adds more.
    #[serde(default)]
    pub communication_extra_days: u32,
    /// Whether resident withholding tax is kept back from late payment
    /// interest. The law decides; off unless the insurer turns it on.
    #[serde(default)]
    pub interest_withholding: bool,
    /// The rate in basis points for a payee who gave no rate (the
    /// no-declaration rate).
    #[serde(default)]
    pub no_declaration_rate_bp: u32,
    /// The consumer price index change, in basis points, that raises a
    /// benefit in payment at a claim anniversary where the cover carries
    /// claim indexation. Set each year from the published index; the rate
    /// in force on the anniversary is the one applied.
    #[serde(default)]
    pub claim_index_bp: u32,
    /// Tax kept back from each income benefit period, in basis points of
    /// the period's benefit. Zero (the default and the placeholder until the
    /// tax review the design names): benefits are paid gross and taxed in
    /// the payee's own return.
    #[serde(default)]
    pub benefit_withholding_bp: u32,
    /// The most paid towards a business mentor for a claimant whose cover
    /// carries the Business Mentors Benefit, over the life of the policy.
    /// The wording pays a two-year membership, one for each business. When
    /// rules leave it out, $1,000 is taken: a best-estimate placeholder until
    /// AIA confirms the figure. Zero: not set, and a request is refused.
    #[serde(default = "business_mentors_placeholder")]
    pub business_mentors_minor: i64,
    /// How often a person paid a benefit is asked to prove they are alive.
    /// Zero: never.
    #[serde(default)]
    pub proof_of_life_months: u32,
    /// Days after the proof falls due before payments are held.
    #[serde(default)]
    pub proof_of_life_grace_days: u32,
    /// Interest on a lump sum paid late, where the law or the product
    /// requires it. Absent: no interest is paid.
    #[serde(default)]
    pub late_interest: Option<LateInterest>,
    /// At or below this an estate is paid on the simplified process.
    pub estate_simplified_max_minor: i64,
    #[serde(default)]
    pub health_benefits: BTreeMap<String, HealthBenefit>,
    pub pre_approval_days: u32,
    /// The same condition recurring within this many days reopens the claim.
    pub recurrence_days: u32,
    pub prepared_by: StaffId,
    pub approved_by: Vec<StaffId>,
}

fn hash_of<T: Serialize>(domain: &str, value: &T) -> String {
    let mut h = Sha256::new();
    h.update(domain.as_bytes());
    // Maps here are BTreeMaps with string or enum keys, so the encoding is canonical.
    h.update(serde_json::to_vec(value).unwrap_or_default());
    hex::encode(h.finalize())
}

fn dual_approval(what: &str, prepared_by: &str, approved_by: &[StaffId]) -> Result<(), Reject> {
    let distinct: BTreeSet<&StaffId> = approved_by.iter().collect();
    if prepared_by.is_empty() || distinct.len() < 2 || approved_by.iter().any(|a| a == prepared_by) {
        return Err(Reject::new(
            "dual_approval",
            format!("{what} need a preparer and two approvers, none of them the preparer"),
        ));
    }
    Ok(())
}

/// The appointed actuary's reserves for claims in payment: the expected
/// remaining payments on each claim, as at a date. A valuation is large, so
/// it arrives in parts; each part is approved like configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Valuation {
    pub version: u32,
    pub part: u32,
    pub as_at: chrono::NaiveDate,
    /// The basis the actuary used, named so the figure can be traced.
    pub basis: String,
    pub reserves: BTreeMap<String, i64>,
    pub prepared_by: StaffId,
    pub approved_by: Vec<StaffId>,
}

/// At most this many claims in one part, so one command stays inside the
/// partition's apply budget.
pub const MAX_VALUATION_PART: usize = 500;

impl Valuation {
    pub fn content_hash(&self) -> String {
        hash_of("ic.claims.valuation.v1", self)
    }

    pub fn validate(&self) -> Result<(), Reject> {
        dual_approval("an actuarial valuation", &self.prepared_by, &self.approved_by)?;
        if self.version == 0 || self.basis.trim().is_empty() || self.reserves.is_empty() || self.reserves.len() > MAX_VALUATION_PART {
            return Err(Reject::new(
                "invalid_valuation",
                format!("a valuation part has a version, a basis and between 1 and {MAX_VALUATION_PART} claims"),
            ));
        }
        if self.reserves.values().any(|r| *r < 0) {
            return Err(Reject::new("invalid_valuation", "a reserve is never negative"));
        }
        Ok(())
    }
}

impl AuthorityBook {
    pub fn content_hash(&self) -> String {
        hash_of("ic.claims.authority.v1", self)
    }

    pub fn validate(&self) -> Result<(), Reject> {
        dual_approval("authority profiles", &self.prepared_by, &self.approved_by)?;
        for (id, s) in &self.staff {
            if !self.profiles.contains_key(&s.profile) {
                return Err(Reject::new(
                    "invalid_authority",
                    format!("{id} holds profile {}, which is not in the book", s.profile),
                ));
            }
        }
        Ok(())
    }

    pub fn staff(&self, id: &str) -> Result<&Staff, Reject> {
        self.staff
            .get(id)
            .ok_or_else(|| Reject::new("unknown_staff", format!("{id} holds no claims role")))
    }

    /// The person's profile. The book is validated at activation, so a
    /// person in it always has one.
    pub fn profile(&self, id: &str) -> Result<&Profile, Reject> {
        let staff = self.staff(id)?;
        self.profiles
            .get(&staff.profile)
            .ok_or_else(|| Reject::new("unknown_staff", format!("{id} holds no authority profile")))
    }

    pub fn has_role(&self, id: &str, role: Role) -> bool {
        self.staff.get(id).is_some_and(|s| s.roles.contains(&role))
    }
}

/// A well-formed cause tag: dotted lowercase segments under a known root.
pub fn tag_recognised(tag: &str) -> bool {
    let mut segments = tag.split('.');
    let root_known = segments.next().is_some_and(|r| CAUSE_ROOTS.contains(&r) && r != "unknown");
    root_known
        && tag
            .split('.')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

/// `tag` is `ancestor` or one of its dotted descendants.
fn tag_under(tag: &str, ancestor: &str) -> bool {
    tag == ancestor || tag.strip_prefix(ancestor).is_some_and(|rest| rest.starts_with('.'))
}

impl Rules {
    /// The cause code is tagged in the category: any of its tags has it as
    /// the top segment.
    pub fn cause_in(&self, cause: Option<&str>, category: &str) -> bool {
        cause
            .and_then(|c| self.cause_codes.get(c))
            .is_some_and(|tags| tags.iter().any(|t| t.split('.').next() == Some(category)))
    }

    pub fn content_hash(&self) -> String {
        hash_of("ic.claims.rules.v1", self)
    }

    pub fn validate(&self) -> Result<(), Reject> {
        dual_approval("claims rules", &self.prepared_by, &self.approved_by)?;
        if self.max_offset_pct > 100 || self.sample_pct.values().any(|p| *p > 100) || self.health_benefits.values().any(|b| b.copay_pct > 100) {
            return Err(Reject::new("invalid_rules", "a percentage is over 100"));
        }
        let tags = self.cause_codes.values().flatten().chain(self.exclusions.values().flat_map(|e| &e.cause_tags));
        if let Some(bad) = tags.into_iter().find(|t| !tag_recognised(t) && t.as_str() != "unknown") {
            return Err(Reject::new("invalid_rules", format!("cause tag {bad} is not under a known root")));
        }
        Ok(())
    }

    pub fn timers(&self, line: Line) -> TimerTargets {
        self.timers.get(&line).copied().unwrap_or_default()
    }

    /// Check a decision's reason codes against the controlled list.
    pub fn check_reasons(&self, kind: DecisionKind, reasons: &[String]) -> Result<(), Reject> {
        if reasons.is_empty() {
            return Err(Reject::new("reason_required", "a decision needs at least one reason code"));
        }
        match reasons.iter().find(|r| !self.reason_codes.get(*r).is_some_and(|uses| uses.contains(&kind))) {
            Some(bad) => Err(Reject::new("unknown_reason", format!("{bad} is not a reason code for this decision"))),
            None => Ok(()),
        }
    }

    /// Whether one exclusion bites on a claim with these cause tags.
    ///
    /// An empty or unrecognised cause never clears an exclusion: a cause
    /// recorded in a form the rules cannot read goes to a person.
    pub fn exclusion_match(&self, code: &str, cover_type: &str, tags: &[String]) -> Match {
        let Some(exclusion) = self.exclusions.get(code) else {
            return Match::NeedsAssessor;
        };
        if exclusion.verbatim {
            return Match::NeedsAssessor;
        }
        // An exclusion that is not about this kind of cover cannot bite,
        // whatever the cause. The underwriting library decides in the same order.
        if !exclusion.applies_to.is_empty() && !exclusion.applies_to.contains(cover_type) {
            return Match::DoesNotApply;
        }
        if tags.is_empty() || tags.iter().any(|t| !tag_recognised(t)) {
            return Match::NeedsAssessor;
        }
        if tags.iter().any(|t| exclusion.cause_tags.iter().any(|x| tag_under(t, x))) {
            Match::Applies
        } else {
            Match::DoesNotApply
        }
    }

    /// The strongest outcome across a cover's exclusions, with each code's own.
    pub fn exclusion_check(&self, codes: &[String], cover_type: &str, cause_code: Option<&str>) -> (Match, BTreeMap<String, Match>) {
        let none = Vec::new();
        let tags = cause_code.and_then(|c| self.cause_codes.get(c)).unwrap_or(&none);
        let each: BTreeMap<String, Match> = codes.iter().map(|c| (c.clone(), self.exclusion_match(c, cover_type, tags))).collect();
        (each.values().copied().max().unwrap_or(Match::DoesNotApply), each)
    }
}

/// The best-estimate placeholder for a two-year Business Mentors membership.
fn business_mentors_placeholder() -> i64 {
    100_000
}
