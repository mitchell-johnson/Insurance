//! What the module holds about a scheme: its terms, its membership as last
//! confirmed by the core, and the changes waiting to be applied.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{LifeId, PartyId};
use serde::{Deserialize, Serialize};

use crate::record::MemberAttrs;

/// A member of a scheme, keyed by the employer's reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub employee_id: String,
    /// The member's life record on the scheme policy in the core.
    pub life: LifeId,
    pub attrs: MemberAttrs,
    /// The first day of cover.
    pub joined: NaiveDate,
    /// The first day without cover, once the member has left.
    #[serde(default)]
    pub left: Option<NaiveDate>,
    /// Set while the member is on a leave of absence.
    #[serde(default)]
    pub absence_since: Option<NaiveDate>,
    /// The person in the customer master, once the member is linked to one.
    #[serde(default)]
    pub party: Option<PartyId>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// A cap on this member's automatic cover, below the scheme's limit.
    #[serde(default)]
    pub auto_cap_minor: Option<i64>,
    /// The plan and tier the member chose from the scheme's health menu.
    /// None is the menu's default plan, single.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthChoice>,
    /// The people the member's health plan covers besides them, each a
    /// life on the scheme policy that holds only the covers chosen for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependants: Vec<Dependant>,
}

/// A member's partner or child on their health plan. The member gives the
/// date of birth, which prices the cover; nothing else about them is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependant {
    /// The member's own name for them: `partner`, `child-1`.
    pub key: String,
    /// `{member's life}+{key}`.
    pub life: LifeId,
    pub date_of_birth: NaiveDate,
    pub joined: NaiveDate,
}

/// A member's choice from the scheme's health menu, from a date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthChoice {
    pub plan: String,
    pub tier: crate::terms::HealthTier,
    pub effective: NaiveDate,
}

impl Member {
    pub fn in_force_on(&self, on: NaiveDate) -> bool {
        self.joined <= on && self.left.is_none_or(|l| l > on)
    }
}

/// What a change does to a member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeKind {
    Join,
    Leave,
    /// A change to category, salary or another attribute.
    Change,
    AbsenceStart,
    AbsenceEnd,
    /// The employer now knows the member by another ID. Nothing changes in
    /// the core: the life record is the same.
    Rekey {
        from: String,
    },
}

impl ChangeKind {
    pub fn code(&self) -> &'static str {
        match self {
            ChangeKind::Join => "join",
            ChangeKind::Leave => "leave",
            ChangeKind::Change => "change",
            ChangeKind::AbsenceStart => "absence_start",
            ChangeKind::AbsenceEnd => "absence_end",
            ChangeKind::Rekey { .. } => "rekey",
        }
    }
}

/// One change to the membership, worked out from a file row or raised by
/// the module itself (a leave of absence running out, a scheme ending).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposed {
    /// The file row it came from. Zero when no row states it: a leaver
    /// derived from a full file, or a change the module raised.
    pub row: u32,
    pub employee_id: String,
    pub kind: ChangeKind,
    /// The value date. For a leaver, the first day without cover.
    pub effective: NaiveDate,
    /// The member's attributes after the change.
    pub attrs: MemberAttrs,
    /// The names of the fields that change. Never their values.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    /// Dated further back than the scheme's terms allow the employer:
    /// needs staff approval before it is applied.
    #[serde(default)]
    pub needs_backdating_approval: bool,
}

/// How a joiner is held in the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinAs {
    pub life: LifeId,
    /// The party the member is linked to from the start, if any.
    pub party: Option<PartyId>,
}

/// The membership of one scheme.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Membership {
    /// The latest spell of membership per employee ID.
    pub members: BTreeMap<String, Member>,
    /// Earlier spells of members who left and later rejoined.
    #[serde(default)]
    pub past: Vec<Member>,
    /// Every life ID ever used on the scheme.
    #[serde(default)]
    lives: BTreeSet<LifeId>,
}

impl Membership {
    /// The member under this ID, if they have not left or been given a
    /// leaving date.
    pub fn active(&self, employee_id: &str) -> Option<&Member> {
        self.members.get(employee_id).filter(|m| m.left.is_none())
    }

    /// Whether this employee was a member before and is not one now.
    pub fn has_left(&self, employee_id: &str) -> bool {
        self.active(employee_id).is_none() && (self.members.contains_key(employee_id) || self.past.iter().any(|m| m.employee_id == employee_id))
    }

    pub fn active_members(&self) -> impl Iterator<Item = &Member> {
        self.members.values().filter(|m| m.left.is_none())
    }

    pub fn active_count(&self) -> usize {
        self.active_members().count()
    }

    pub fn by_life(&self, life: &LifeId) -> Option<&Member> {
        self.members.values().chain(self.past.iter()).find(|m| &m.life == life)
    }

    /// The member a life belongs to: the member's own, or a dependant's.
    pub fn holder(&self, life: &LifeId) -> Option<&Member> {
        self.by_life(life).or_else(|| {
            self.members
                .values()
                .chain(self.past.iter())
                .find(|m| m.dependants.iter().any(|d| &d.life == life))
        })
    }

    pub fn by_life_mut(&mut self, life: &LifeId) -> Option<&mut Member> {
        self.members.values_mut().chain(self.past.iter_mut()).find(|m| &m.life == life)
    }

    /// A life ID not yet used on the scheme for a new spell of this member.
    pub fn next_life(&self, employee_id: &str) -> LifeId {
        let first = LifeId(employee_id.to_string());
        if !self.lives.contains(&first) {
            return first;
        }
        let mut n = 2u32;
        loop {
            let candidate = LifeId(format!("{employee_id}~{n}"));
            if !self.lives.contains(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Add a member directly: the members a scheme was issued with.
    pub fn insert(&mut self, member: Member) {
        self.lives.insert(member.life.clone());
        if let Some(previous) = self.members.insert(member.employee_id.clone(), member) {
            self.past.push(previous);
        }
    }

    /// Undo a leave: the member is back with no gap. False if the member
    /// under this ID has not left.
    pub fn reinstate(&mut self, employee_id: &str) -> bool {
        self.members.get_mut(employee_id).filter(|m| m.left.is_some()).map(|m| m.left = None).is_some()
    }

    /// Apply a change. Returns false, changing nothing, if the change does
    /// not fit the membership: the member it names is not there, or a join
    /// arrives without the life it joins as.
    pub fn apply(&mut self, change: &Proposed, join: Option<&JoinAs>) -> bool {
        match &change.kind {
            ChangeKind::Join => {
                let Some(join) = join else { return false };
                if self.active(&change.employee_id).is_some() {
                    return false;
                }
                self.insert(Member {
                    employee_id: change.employee_id.clone(),
                    life: join.life.clone(),
                    attrs: change.attrs.clone(),
                    joined: change.effective,
                    left: None,
                    absence_since: None,
                    party: join.party.clone(),
                    fingerprint: change.fingerprint.clone(),
                    auto_cap_minor: None,
                    health: None,
                    dependants: vec![],
                });
                true
            }
            ChangeKind::Rekey { from } => {
                if self.active(from).is_none() || self.members.contains_key(&change.employee_id) {
                    return false;
                }
                let moved = self.members.remove(from).map(|mut member| {
                    member.employee_id = change.employee_id.clone();
                    self.members.insert(change.employee_id.clone(), member);
                });
                moved.is_some()
            }
            other => {
                let Some(member) = self.members.get_mut(&change.employee_id).filter(|m| m.left.is_none()) else {
                    return false;
                };
                match other {
                    ChangeKind::Leave => member.left = Some(change.effective),
                    ChangeKind::AbsenceStart => member.absence_since = Some(change.effective),
                    ChangeKind::AbsenceEnd => member.absence_since = None,
                    _ => member.attrs = change.attrs.clone(),
                }
                true
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::benefit::tests::attrs;
    use crate::terms::tests::d;

    pub(crate) fn proposed(id: &str, kind: ChangeKind, effective: NaiveDate) -> Proposed {
        Proposed {
            row: 1,
            employee_id: id.to_string(),
            kind,
            effective,
            attrs: attrs("staff", 80_000_00),
            fields: vec![],
            fingerprint: None,
            needs_backdating_approval: false,
        }
    }

    pub(crate) fn join_as(m: &Membership, id: &str) -> JoinAs {
        JoinAs {
            life: m.next_life(id),
            party: None,
        }
    }

    /// Purpose: a member joins, changes, goes on leave and returns, is re-keyed, leaves and rejoins;
    /// each step changes exactly what it should, and the earlier spell is kept under its own life.
    /// Why it matters: this read model is what files are compared against and what is reconciled
    /// to the core. A rejoiner who overwrote their first spell would lose the history a late claim
    /// on that spell needs.
    #[test]
    fn membership_follows_a_member_through_every_change() {
        let mut m = Membership::default();
        let j = join_as(&m, "E1");
        assert_eq!(j.life, LifeId::from("E1"));
        assert!(m.apply(&proposed("E1", ChangeKind::Join, d(2027, 11, 1)), Some(&j)));
        assert_eq!(m.active_count(), 1);
        assert!(m.active("E1").unwrap().in_force_on(d(2027, 11, 1)));
        assert!(!m.active("E1").unwrap().in_force_on(d(2027, 10, 31)));

        let mut change = proposed("E1", ChangeKind::Change, d(2027, 12, 1));
        change.attrs.salary_minor = Some(90_000_00);
        assert!(m.apply(&change, None));
        assert_eq!(m.active("E1").unwrap().attrs.salary_minor, Some(90_000_00));

        assert!(m.apply(&proposed("E1", ChangeKind::AbsenceStart, d(2028, 1, 10)), None));
        assert_eq!(m.active("E1").unwrap().absence_since, Some(d(2028, 1, 10)));
        assert!(m.apply(&proposed("E1", ChangeKind::AbsenceEnd, d(2028, 2, 1)), None));
        assert_eq!(m.active("E1").unwrap().absence_since, None);

        assert!(m.apply(&proposed("N1", ChangeKind::Rekey { from: "E1".to_string() }, d(2028, 2, 1)), None));
        assert!(m.active("E1").is_none());
        assert_eq!(m.active("N1").unwrap().life, LifeId::from("E1"));
        assert_eq!(m.by_life(&LifeId::from("E1")).unwrap().employee_id, "N1");

        assert!(m.apply(&proposed("N1", ChangeKind::Leave, d(2028, 3, 1)), None));
        assert!(m.active("N1").is_none());
        assert_eq!(m.active_count(), 0);
        assert!(m.members["N1"].in_force_on(d(2028, 2, 29)) && !m.members["N1"].in_force_on(d(2028, 3, 1)));

        // Rejoining under the original ID: the life ID E1 is taken, so the new spell gets another.
        let again = join_as(&m, "E1");
        assert_eq!(again.life, LifeId::from("E1~2"));
        assert!(m.apply(&proposed("E1", ChangeKind::Join, d(2028, 6, 1)), Some(&again)));
        let third = join_as(&m, "E1");
        assert_eq!(third.life, LifeId::from("E1~3"));
        // Rejoining under the later ID keeps the first spell in the past list.
        let n1 = join_as(&m, "N1");
        assert!(m.apply(&proposed("N1", ChangeKind::Join, d(2028, 7, 1)), Some(&n1)));
        assert_eq!(m.past.len(), 1);
        assert_eq!(m.by_life(&LifeId::from("E1")).unwrap().left, Some(d(2028, 3, 1)));
        m.by_life_mut(&LifeId::from("E1")).unwrap().party = Some(PartyId::from("p1"));
        assert_eq!(m.past[0].party, Some(PartyId::from("p1")));
        assert!(m.by_life(&LifeId::from("nobody")).is_none());
    }

    /// Purpose: a change that does not fit the membership is refused and changes nothing: a join
    /// without a life or for a current member, a change, leave or absence for someone who is not a
    /// member, a re-key from an unknown ID or onto an ID in use.
    /// Why it matters: the read model is updated only from what the core accepted, but a bug or a
    /// replayed result must not be able to invent or duplicate a member.
    #[test]
    fn changes_that_do_not_fit_are_refused() {
        let mut m = Membership::default();
        assert!(!m.apply(&proposed("E1", ChangeKind::Join, d(2027, 11, 1)), None));
        let j = join_as(&m, "E1");
        assert!(m.apply(&proposed("E1", ChangeKind::Join, d(2027, 11, 1)), Some(&j)));
        let j2 = join_as(&m, "E2");
        assert!(m.apply(&proposed("E2", ChangeKind::Join, d(2027, 11, 1)), Some(&j2)));
        let before = m.clone();
        assert!(!m.apply(&proposed("E1", ChangeKind::Join, d(2027, 12, 1)), Some(&j)));
        for kind in [ChangeKind::Leave, ChangeKind::Change, ChangeKind::AbsenceStart, ChangeKind::AbsenceEnd] {
            assert!(!m.apply(&proposed("E9", kind, d(2027, 12, 1)), None));
        }
        assert!(!m.apply(&proposed("E3", ChangeKind::Rekey { from: "E9".to_string() }, d(2027, 12, 1)), None));
        assert!(!m.apply(&proposed("E2", ChangeKind::Rekey { from: "E1".to_string() }, d(2027, 12, 1)), None));
        assert_eq!(m, before);
        let codes: Vec<_> = [
            ChangeKind::Join,
            ChangeKind::Leave,
            ChangeKind::Change,
            ChangeKind::AbsenceStart,
            ChangeKind::AbsenceEnd,
            ChangeKind::Rekey { from: String::new() },
        ]
        .iter()
        .map(ChangeKind::code)
        .collect();
        assert_eq!(codes, vec!["join", "leave", "change", "absence_start", "absence_end", "rekey"]);
    }
}
