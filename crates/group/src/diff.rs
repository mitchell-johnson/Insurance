//! Working out what a file changes. A full file is compared with the
//! membership in force to derive joiners, leavers and changes; a change file
//! states them directly. Matching is by employee ID within the scheme.
//! Anything ambiguous becomes an exception item for a person. Nothing is
//! guessed.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Days, NaiveDate};
use ic_product::ProductDefinition;
use serde::{Deserialize, Serialize};

use crate::benefit::{check_attrs, check_join, Ineligible};
use crate::mapping::{FileKind, RowReject};
use crate::model::{ChangeKind, JoinAs, Membership, Proposed};
use crate::record::{MemberRow, RowChange};
use crate::terms::{MemberBasis, SchemeTerms, CORE_BACKDATING_LIMIT_DAYS};

/// Something in a file that a person has to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Ambiguity {
    /// The same employee ID on more than one row of a full file.
    DuplicateId { employee_id: String },
    /// A new ID appears and a member with the same name and date of birth
    /// disappears: probably one person whose ID changed.
    PossibleIdChange { employee_id: String, other: String },
    /// A joiner has the same name and date of birth as a current member, or
    /// as another joiner in the file.
    LikelyDuplicate { employee_id: String, other: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Derived {
    pub changes: Vec<Proposed>,
    pub rejected: Vec<RowReject>,
    pub ambiguities: Vec<Ambiguity>,
}

impl Derived {
    pub fn count(&self, kind: &ChangeKind) -> usize {
        self.changes.iter().filter(|c| &c.kind == kind).count()
    }
}

pub struct DiffContext<'a> {
    pub terms: &'a SchemeTerms,
    pub product: &'a ProductDefinition,
    pub inception: NaiveDate,
    pub today: NaiveDate,
    /// The date the file speaks for.
    pub file_date: NaiveDate,
}

fn reject(row: &MemberRow, (code, field): Ineligible) -> RowReject {
    RowReject::new(row.row, Some(&row.employee_id), code, Some(field))
}

impl DiffContext<'_> {
    /// Check a value date against the scheme's start and the backdating
    /// limits. Returns whether staff approval is needed.
    fn check_date(&self, effective: NaiveDate) -> Result<bool, Ineligible> {
        if effective < self.inception {
            return Err(("before_inception", "effective"));
        }
        let back = |days: u32| self.today.checked_sub_days(Days::new(u64::from(days))).unwrap_or(NaiveDate::MIN);
        if effective < back(CORE_BACKDATING_LIMIT_DAYS) {
            return Err(("backdating_limit", "effective"));
        }
        Ok(effective < back(self.terms.backdating_limit_days))
    }

    /// The first day of cover for a joiner: the date the row states,
    /// otherwise the day the waiting period ends (not before the scheme
    /// started), otherwise the file's date.
    fn join_date(&self, row: &MemberRow) -> NaiveDate {
        row.effective.unwrap_or_else(|| {
            row.attrs
                .employment_start
                .and_then(|s| s.checked_add_days(Days::new(u64::from(self.terms.eligibility.waiting_days))))
                .map_or(self.file_date, |eligible| eligible.max(self.inception))
        })
    }

    fn join(&self, row: &MemberRow) -> Result<Proposed, Ineligible> {
        if self.terms.basis == MemberBasis::Named && row.fingerprint.is_none() {
            return Err(("missing_value", "name"));
        }
        let effective = self.join_date(row);
        let mut attrs = row.attrs.clone();
        if attrs.billing_group.is_none() {
            attrs.billing_group = Some(self.terms.default_billing_group().to_string());
        }
        check_join(self.terms, self.product, &attrs, effective)?;
        Ok(Proposed {
            row: row.row,
            employee_id: row.employee_id.clone(),
            kind: ChangeKind::Join,
            effective,
            attrs,
            fields: vec![],
            fingerprint: row.fingerprint.clone(),
            needs_backdating_approval: self.check_date(effective)?,
        })
    }

    /// A change to a current member, if the row differs from what is held.
    fn change(&self, view: &Membership, row: &MemberRow, employee_id: &str) -> Result<Option<Proposed>, Ineligible> {
        let Some(held) = view.active(employee_id) else {
            return Err(("unknown_member", "employee_id"));
        };
        let fields = held.attrs.changed_fields(&row.attrs);
        if fields.is_empty() {
            return Ok(None);
        }
        let effective = row.effective.unwrap_or(self.file_date);
        if effective < held.joined {
            return Err(("before_joining", "effective"));
        }
        let attrs = held.attrs.merged(&row.attrs);
        check_attrs(self.terms, self.product, &attrs)?;
        Ok(Some(Proposed {
            row: row.row,
            employee_id: row.employee_id.clone(),
            kind: ChangeKind::Change,
            effective,
            attrs,
            fields: fields.into_iter().map(str::to_string).collect(),
            fingerprint: row.fingerprint.clone().or_else(|| held.fingerprint.clone()),
            needs_backdating_approval: self.check_date(effective)?,
        }))
    }

    /// A member leaving on `left_on`: cover ends as the terms say.
    fn leave(&self, view: &Membership, row_number: u32, employee_id: &str, left_on: NaiveDate) -> Result<Proposed, Ineligible> {
        let Some(held) = view.active(employee_id) else {
            return Err(("unknown_member", "employee_id"));
        };
        let effective = self.terms.cover_end_for(left_on);
        if effective < held.joined {
            return Err(("before_joining", "effective"));
        }
        Ok(Proposed {
            row: row_number,
            employee_id: employee_id.to_string(),
            kind: ChangeKind::Leave,
            effective,
            attrs: held.attrs.clone(),
            fields: vec![],
            fingerprint: held.fingerprint.clone(),
            needs_backdating_approval: self.check_date(effective)?,
        })
    }

    fn absence(&self, view: &Membership, row: &MemberRow, start: bool) -> Result<Proposed, Ineligible> {
        let Some(held) = view.active(&row.employee_id) else {
            return Err(("unknown_member", "employee_id"));
        };
        match (start, held.absence_since) {
            (true, Some(_)) => return Err(("already_absent", "change")),
            (false, None) => return Err(("not_absent", "change")),
            _ => {}
        }
        let effective = row.effective.unwrap_or(self.file_date);
        if effective < held.joined {
            return Err(("before_joining", "effective"));
        }
        Ok(Proposed {
            row: row.row,
            employee_id: row.employee_id.clone(),
            kind: if start { ChangeKind::AbsenceStart } else { ChangeKind::AbsenceEnd },
            effective,
            attrs: held.attrs.clone(),
            fields: vec![],
            fingerprint: held.fingerprint.clone(),
            needs_backdating_approval: false,
        })
    }

    /// The employer says the member under `from` is now known by the row's
    /// ID. Returns the re-key and, if the row also changes attributes, the
    /// change that follows it.
    fn rekey(&self, view: &Membership, row: &MemberRow, from: &str) -> Result<Vec<Proposed>, Ineligible> {
        let Some(held) = view.active(from) else {
            return Err(("unknown_previous_id", "previous_employee_id"));
        };
        let rekey = Proposed {
            row: row.row,
            employee_id: row.employee_id.clone(),
            kind: ChangeKind::Rekey { from: from.to_string() },
            effective: self.file_date,
            attrs: held.attrs.clone(),
            fields: vec!["employee_id".to_string()],
            fingerprint: held.fingerprint.clone(),
            needs_backdating_approval: false,
        };
        let mut out = vec![rekey];
        if let Some(change) = self.change(view, row, from)? {
            out.push(change);
        }
        Ok(out)
    }
}

/// Members by fingerprint, for noticing one person under two IDs.
fn fingerprints(view: &Membership) -> BTreeMap<&str, &str> {
    view.active_members()
        .filter_map(|m| Some((m.fingerprint.as_deref()?, m.employee_id.as_str())))
        .collect()
}

fn derive_full(ctx: &DiffContext<'_>, view: &Membership, rows: &[MemberRow]) -> Derived {
    let mut out = Derived::default();
    let mut seen: BTreeMap<&str, u32> = BTreeMap::new();
    for r in rows {
        *seen.entry(r.employee_id.as_str()).or_insert(0) += 1;
    }
    for (id, _) in seen.iter().filter(|(_, n)| **n > 1) {
        out.ambiguities.push(Ambiguity::DuplicateId {
            employee_id: (*id).to_string(),
        });
    }
    // Everyone the file accounts for: by their own ID, or as the previous ID
    // of a row that re-keys them.
    let mut present: BTreeSet<&str> = seen.keys().copied().collect();
    let mut leavers: BTreeSet<&str> = BTreeSet::new();
    let by_fingerprint = fingerprints(view);
    let mut joiners: Vec<Proposed> = Vec::new();

    for row in rows {
        if seen.get(row.employee_id.as_str()).copied().unwrap_or(0) > 1 {
            out.rejected.push(reject(row, ("duplicate_employee_id", "employee_id")));
            continue;
        }
        let previous = row.previous_employee_id.as_deref().filter(|p| *p != row.employee_id);
        let result = if view.active(&row.employee_id).is_some() {
            ctx.change(view, row, &row.employee_id).map(|c| c.into_iter().collect())
        } else if let Some(from) = previous {
            present.insert(from);
            ctx.rekey(view, row, from)
        } else {
            ctx.join(row).map(|j| {
                joiners.push(j);
                vec![]
            })
        };
        match result {
            Ok(changes) => out.changes.extend(changes),
            Err(why) => out.rejected.push(reject(row, why)),
        }
    }
    for m in view.active_members() {
        if !present.contains(m.employee_id.as_str()) {
            leavers.insert(m.employee_id.as_str());
        }
    }

    // A joiner who looks like someone already here is never applied on a
    // guess. If that someone is absent from the file it is probably an ID
    // change, and neither the leaver nor the joiner is applied.
    let mut joiner_prints: BTreeMap<&str, &str> = BTreeMap::new();
    let mut held: BTreeSet<String> = BTreeSet::new();
    for j in &joiners {
        let Some(print) = j.fingerprint.as_deref() else { continue };
        if let Some(other) = by_fingerprint.get(print) {
            if leavers.remove(other) {
                out.ambiguities.push(Ambiguity::PossibleIdChange {
                    employee_id: j.employee_id.clone(),
                    other: (*other).to_string(),
                });
            } else {
                out.ambiguities.push(Ambiguity::LikelyDuplicate {
                    employee_id: j.employee_id.clone(),
                    other: (*other).to_string(),
                });
            }
            held.insert(j.employee_id.clone());
        } else if let Some(other) = joiner_prints.insert(print, j.employee_id.as_str()) {
            out.ambiguities.push(Ambiguity::LikelyDuplicate {
                employee_id: j.employee_id.clone(),
                other: other.to_string(),
            });
            held.insert(j.employee_id.clone());
            held.insert(other.to_string());
        }
    }
    for j in joiners {
        if held.contains(&j.employee_id) {
            out.rejected
                .push(RowReject::new(j.row, Some(&j.employee_id), "held_for_review", Some("employee_id")));
        } else {
            out.changes.push(j);
        }
    }
    for id in leavers {
        match ctx.leave(view, 0, id, ctx.file_date) {
            Ok(leave) => out.changes.push(leave),
            Err((code, field)) => out.rejected.push(RowReject::new(0, Some(id), code, Some(field))),
        }
    }
    out
}

fn derive_changes(ctx: &DiffContext<'_>, view: &Membership, rows: &[MemberRow]) -> Derived {
    let mut out = Derived::default();
    // Later rows see earlier ones: a file may hire and then change someone.
    let mut view = view.clone();
    for row in rows {
        let previous = row.previous_employee_id.as_deref().filter(|p| *p != row.employee_id);
        let result: Result<Vec<Proposed>, Ineligible> = match row.change {
            Some(RowChange::Join) => {
                if view.active(&row.employee_id).is_some() {
                    Err(("already_member", "employee_id"))
                } else {
                    ctx.join(row).and_then(|j| {
                        let twin = j.fingerprint.as_deref().and_then(|p| fingerprints(&view).get(p).map(|o| (*o).to_string()));
                        match twin {
                            Some(other) => {
                                out.ambiguities.push(Ambiguity::LikelyDuplicate {
                                    employee_id: row.employee_id.clone(),
                                    other,
                                });
                                Err(("held_for_review", "employee_id"))
                            }
                            None => Ok(vec![j]),
                        }
                    })
                }
            }
            Some(RowChange::Leave) => ctx
                .leave(&view, row.row, &row.employee_id, row.effective.unwrap_or(ctx.file_date))
                .map(|l| vec![l]),
            Some(RowChange::Change) => match previous {
                Some(from) if view.active(&row.employee_id).is_none() => ctx.rekey(&view, row, from),
                _ => ctx
                    .change(&view, row, &row.employee_id)
                    .and_then(|c| c.map(|c| vec![c]).ok_or(("no_change", "change"))),
            },
            Some(RowChange::AbsenceStart) => ctx.absence(&view, row, true).map(|a| vec![a]),
            Some(RowChange::AbsenceEnd) => ctx.absence(&view, row, false).map(|a| vec![a]),
            None => Err(("missing_value", "change")),
        };
        match result {
            Ok(changes) => {
                for c in changes {
                    let join = JoinAs {
                        life: view.next_life(&c.employee_id),
                        party: None,
                    };
                    view.apply(&c, Some(&join));
                    out.changes.push(c);
                }
            }
            Err(why) => out.rejected.push(reject(row, why)),
        }
    }
    out
}

/// Work out what a file changes against the membership `view`.
pub fn derive(ctx: &DiffContext<'_>, view: &Membership, kind: FileKind, rows: &[MemberRow]) -> Derived {
    match kind {
        FileKind::Full => derive_full(ctx, view, rows),
        FileKind::Changes => derive_changes(ctx, view, rows),
    }
}

/// Whether a file removes more of the membership than the scheme's safety
/// share allows without explicit confirmation. A truncated file looks
/// exactly like mass resignations.
pub fn trips_safety_check(terms: &SchemeTerms, active_before: usize, leavers: usize) -> bool {
    active_before > 0 && leavers * 100 > usize::from(terms.safety_share_percent) * active_before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benefit::tests::{attrs, group_life};
    use crate::model::tests::{join_as, proposed};
    use crate::record::MemberAttrs;
    use crate::terms::tests::{d, sample_terms};
    use crate::terms::CoverEnd;

    fn row(n: u32, id: &str, print: Option<&str>) -> MemberRow {
        MemberRow {
            row: n,
            employee_id: id.to_string(),
            change: None,
            effective: None,
            attrs: attrs("staff", 80_000_00),
            previous_employee_id: None,
            fingerprint: print.map(str::to_string),
        }
    }

    fn event(n: u32, id: &str, change: RowChange, effective: Option<NaiveDate>) -> MemberRow {
        MemberRow {
            change: Some(change),
            effective,
            attrs: MemberAttrs::default(),
            ..row(n, id, None)
        }
    }

    /// Three members from 1 November 2027: E1 (print a), E2 (print b), E3 (no print).
    fn view() -> Membership {
        let mut m = Membership::default();
        for (id, print) in [("E1", Some("a")), ("E2", Some("b")), ("E3", None)] {
            let mut p = proposed(id, ChangeKind::Join, d(2027, 11, 1));
            p.fingerprint = print.map(str::to_string);
            let j = join_as(&m, id);
            assert!(m.apply(&p, Some(&j)));
        }
        m
    }

    struct Fixture {
        terms: SchemeTerms,
        product: ProductDefinition,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                terms: sample_terms(),
                product: group_life(),
            }
        }
        fn ctx(&self) -> DiffContext<'_> {
            DiffContext {
                terms: &self.terms,
                product: &self.product,
                inception: d(2027, 11, 1),
                today: d(2027, 12, 5),
                file_date: d(2027, 12, 1),
            }
        }
    }

    fn summary(out: &Derived) -> Vec<(String, &'static str, NaiveDate)> {
        out.changes.iter().map(|c| (c.employee_id.clone(), c.kind.code(), c.effective)).collect()
    }

    fn rejects(out: &Derived) -> Vec<(Option<&str>, &str)> {
        out.rejected.iter().map(|r| (r.employee_id.as_deref(), r.code.as_str())).collect()
    }

    /// Purpose: a full file is compared with the membership in force: a member whose salary
    /// differs is a change naming the field, an unknown ID is a joiner, a member missing from the
    /// file is a leaver from the file's date, and an unchanged member produces nothing.
    /// Why it matters: this is the heart of intake. Most employers send the whole list every month,
    /// and what they mean by it is exactly this comparison.
    #[test]
    fn a_full_file_yields_joiners_leavers_and_changes() {
        let f = Fixture::new();
        let mut changed = row(2, "E2", Some("b"));
        changed.attrs.salary_minor = Some(95_000_00);
        let mut joiner = row(3, "E4", Some("d"));
        joiner.attrs.billing_group = None;
        joiner.attrs.employment_start = Some(d(2027, 11, 20));
        let rows = vec![row(1, "E1", Some("a")), changed, joiner];
        let out = derive(&f.ctx(), &view(), FileKind::Full, &rows);
        assert_eq!(
            summary(&out),
            vec![
                ("E2".to_string(), "change", d(2027, 12, 1)),
                // Cover starts on the joiner's first day of work: no waiting period here.
                ("E4".to_string(), "join", d(2027, 11, 20)),
                ("E3".to_string(), "leave", d(2027, 12, 1)),
            ]
        );
        assert_eq!(out.changes[0].fields, vec!["salary"]);
        assert_eq!(out.changes[0].attrs.salary_minor, Some(95_000_00));
        assert_eq!(out.changes[0].attrs.category.as_deref(), Some("staff"));
        // A joiner with no site goes to the scheme's first billing group.
        assert_eq!(out.changes[1].attrs.billing_group.as_deref(), Some("akl"));
        assert_eq!(out.changes[2].row, 0);
        assert!(out.rejected.is_empty() && out.ambiguities.is_empty());
        assert_eq!((out.count(&ChangeKind::Join), out.count(&ChangeKind::Leave)), (1, 1));
    }

    /// Purpose: the joiner's first day of cover is the stated date if there is one, otherwise the
    /// end of the waiting period but never before the scheme started, otherwise the file's date.
    /// Why it matters: the start date sets both the first premium and whether an early claim is
    /// covered. It has to follow a fixed rule the employer can be told, not a guess per file.
    #[test]
    fn a_joiners_cover_starts_by_rule() {
        let mut f = Fixture::new();
        f.terms.eligibility.waiting_days = 10;
        let mut stated = row(1, "E4", Some("d"));
        stated.effective = Some(d(2027, 12, 3));
        let mut waiting = row(2, "E5", Some("e"));
        waiting.attrs.employment_start = Some(d(2027, 11, 15));
        let mut long_service = row(3, "E6", Some("f"));
        long_service.attrs.employment_start = Some(d(2019, 1, 1));
        let rows = vec![row(9, "E1", None), row(8, "E2", None), row(7, "E3", None), stated, waiting, long_service];
        let out = derive(&f.ctx(), &view(), FileKind::Full, &rows);
        assert_eq!(
            summary(&out),
            vec![
                ("E4".to_string(), "join", d(2027, 12, 3)),
                ("E5".to_string(), "join", d(2027, 11, 25)),
                ("E6".to_string(), "join", d(2027, 11, 1)),
            ]
        );
        // With no start date in the file at all, cover starts on the file's date.
        f.terms.eligibility.waiting_days = 0;
        let mut bare = row(1, "E7", Some("g"));
        bare.attrs.employment_start = None;
        let out = derive(&f.ctx(), &Membership::default(), FileKind::Full, &[bare]);
        assert_eq!(summary(&out), vec![("E7".to_string(), "join", d(2027, 12, 1))]);
    }

    /// Purpose: the same ID on two rows rejects both rows and raises one exception; a joiner with
    /// the fingerprint of a member missing from the file is a possible ID change and neither the
    /// leaver nor the joiner is applied; a joiner with the fingerprint of a member still present,
    /// or of another joiner, is a likely duplicate and is held.
    /// Why it matters: "anything ambiguous becomes an exception item; nothing is guessed". Guessing
    /// an ID change wrongly ends one person's cover and gives it to another. Applying a duplicate
    /// bills the employer twice and double-counts the life for reinsurance.
    #[test]
    fn ambiguous_rows_are_held_for_a_person() {
        let f = Fixture::new();
        let rows = vec![
            row(1, "E1", Some("a")),
            row(2, "E1", Some("a")),
            // E2 is missing; N2 has E2's fingerprint.
            row(3, "N2", Some("b")),
            row(4, "E3", None),
            // N1 has the fingerprint of E1, who is still in the file.
            row(5, "N1", Some("a")),
            // Two joiners with the same new fingerprint.
            row(6, "N3", Some("z")),
            row(7, "N4", Some("z")),
            // A joiner with no fingerprint cannot join a named scheme.
            row(8, "N5", None),
            row(9, "N6", Some("y")),
        ];
        let out = derive(&f.ctx(), &view(), FileKind::Full, &rows);
        assert_eq!(
            out.ambiguities,
            vec![
                Ambiguity::DuplicateId { employee_id: "E1".to_string() },
                Ambiguity::PossibleIdChange {
                    employee_id: "N2".to_string(),
                    other: "E2".to_string()
                },
                Ambiguity::LikelyDuplicate {
                    employee_id: "N1".to_string(),
                    other: "E1".to_string()
                },
                Ambiguity::LikelyDuplicate {
                    employee_id: "N4".to_string(),
                    other: "N3".to_string()
                },
            ]
        );
        assert_eq!(
            rejects(&out),
            vec![
                (Some("E1"), "duplicate_employee_id"),
                (Some("E1"), "duplicate_employee_id"),
                (Some("N5"), "missing_value"),
                (Some("N2"), "held_for_review"),
                (Some("N1"), "held_for_review"),
                (Some("N3"), "held_for_review"),
                (Some("N4"), "held_for_review"),
            ]
        );
        // Only the unambiguous joiner is applied. E1 and E2 are not derived as leavers.
        assert_eq!(summary(&out), vec![("N6".to_string(), "join", d(2027, 11, 1))]);
    }

    /// Purpose: when the file itself names a member's previous ID the member is re-keyed, with any
    /// attribute change following; a previous ID nobody holds rejects the row; and once re-keyed,
    /// a later file that still carries the old ID column changes nothing.
    /// Why it matters: an employer changing payroll systems re-numbers everyone. Stated outright it
    /// is safe to apply, and must not be read as everyone leaving and joining again, which would
    /// restart waiting periods and trigger continuation offers.
    #[test]
    fn a_stated_previous_id_rekeys_the_member() {
        let f = Fixture::new();
        let mut moved = row(1, "N1", Some("a"));
        moved.previous_employee_id = Some("E1".to_string());
        moved.attrs.category = Some("executive".to_string());
        let mut unknown = row(2, "N9", Some("q"));
        unknown.previous_employee_id = Some("E9".to_string());
        let mut same = row(3, "E2", Some("b"));
        same.previous_employee_id = Some("E2".to_string());
        let rows = vec![moved.clone(), unknown, same, row(4, "E3", None)];
        let out = derive(&f.ctx(), &view(), FileKind::Full, &rows);
        assert_eq!(
            summary(&out),
            vec![("N1".to_string(), "rekey", d(2027, 12, 1)), ("N1".to_string(), "change", d(2027, 12, 1))]
        );
        assert_eq!(out.changes[0].kind, ChangeKind::Rekey { from: "E1".to_string() });
        assert_eq!(out.changes[1].fields, vec!["category"]);
        assert_eq!(rejects(&out), vec![(Some("N9"), "unknown_previous_id")]);

        // Next month: N1 is the held ID, and the old ID column is still there.
        let mut after = view();
        assert!(after.apply(&out.changes[0], None));
        assert!(after.apply(&out.changes[1], None));
        let next = derive(&f.ctx(), &after, FileKind::Full, &[moved, row(2, "E2", Some("b")), row(3, "E3", None)]);
        assert!(next.changes.is_empty() && next.rejected.is_empty(), "{next:?}");
    }

    /// Purpose: dates are held to the scheme's start and the two backdating limits: before the
    /// scheme started or beyond the core's limit is rejected, beyond the scheme's own limit is
    /// flagged for staff approval, and a change or leave dated before the member joined is
    /// rejected.
    /// Why it matters: backdating moves money for periods already billed. The employer's own limit
    /// stops routine abuse, the approval tier lets genuine late notifications through, and the hard
    /// limit is the one the core will enforce regardless.
    #[test]
    fn dates_are_held_to_the_backdating_limits() {
        let f = Fixture::new();
        let mut ctx = f.ctx();
        ctx.inception = d(2026, 1, 1);
        ctx.today = d(2027, 12, 5);
        let mut m = Membership::default();
        let j = join_as(&m, "E1");
        assert!(m.apply(&proposed("E1", ChangeKind::Join, d(2027, 6, 1)), Some(&j)));
        let stated = |n: u32, id: &str, on: NaiveDate| {
            let mut r = row(n, id, Some(id));
            r.effective = Some(on);
            r
        };
        let mut e1 = stated(1, "E1", d(2027, 5, 1));
        e1.attrs.salary_minor = Some(1);
        let rows = vec![
            e1,
            stated(2, "J1", d(2025, 12, 31)),
            stated(3, "J2", d(2026, 10, 1)),
            stated(4, "J3", d(2027, 8, 1)),
            stated(5, "J4", d(2027, 9, 6)),
        ];
        let out = derive(&ctx, &m, FileKind::Full, &rows);
        assert_eq!(
            rejects(&out),
            vec![
                (Some("E1"), "before_joining"),
                (Some("J1"), "before_inception"),
                (Some("J2"), "backdating_limit")
            ]
        );
        let flags: Vec<_> = out.changes.iter().map(|c| (c.employee_id.as_str(), c.needs_backdating_approval)).collect();
        assert_eq!(flags, vec![("J3", true), ("J4", false)]);

        // A change file leave dated before the member joined, and a derived leaver whose cover
        // would end before they joined (the file speaks for a date before their start).
        let leave = event(1, "E1", RowChange::Leave, Some(d(2027, 5, 1)));
        let out = derive(&ctx, &m, FileKind::Changes, &[leave]);
        assert_eq!(rejects(&out), vec![(Some("E1"), "before_joining")]);
        ctx.file_date = d(2027, 5, 1);
        let out = derive(&ctx, &m, FileKind::Full, &[]);
        assert_eq!(rejects(&out), vec![(Some("E1"), "before_joining")]);
        assert_eq!(out.rejected[0].row, 0);
    }

    /// Purpose: a change file's rows are applied in order, each seeing the ones before it: a hire
    /// followed by a pay change, a leave of absence and a return, a leaver, with cover ending at
    /// the end of the month where the terms say so.
    /// Why it matters: a payroll feed is a sequence of events. Reading them out of order, or each
    /// against the stale membership, would reject the pay change for someone hired two rows
    /// earlier.
    #[test]
    fn a_change_file_is_read_in_order() {
        let mut f = Fixture::new();
        f.terms.cover_end = CoverEnd::EndOfMonth;
        let mut hire = event(1, "E4", RowChange::Join, Some(d(2027, 12, 1)));
        hire.attrs = attrs("staff", 70_000_00);
        hire.fingerprint = Some("d".to_string());
        let mut raise = event(2, "E4", RowChange::Change, Some(d(2027, 12, 3)));
        raise.attrs.salary_minor = Some(75_000_00);
        let rows = vec![
            hire,
            raise,
            event(3, "E1", RowChange::AbsenceStart, Some(d(2027, 12, 2))),
            event(4, "E1", RowChange::AbsenceEnd, None),
            event(5, "E2", RowChange::Leave, Some(d(2027, 12, 14))),
            event(6, "E3", RowChange::Leave, None),
        ];
        let out = derive(&f.ctx(), &view(), FileKind::Changes, &rows);
        assert!(out.rejected.is_empty(), "{:?}", out.rejected);
        assert_eq!(
            summary(&out),
            vec![
                ("E4".to_string(), "join", d(2027, 12, 1)),
                ("E4".to_string(), "change", d(2027, 12, 3)),
                ("E1".to_string(), "absence_start", d(2027, 12, 2)),
                ("E1".to_string(), "absence_end", d(2027, 12, 1)),
                ("E2".to_string(), "leave", d(2028, 1, 1)),
                ("E3".to_string(), "leave", d(2028, 1, 1)),
            ]
        );
        assert_eq!(out.changes[1].attrs.salary_minor, Some(75_000_00));
        assert_eq!(out.changes[1].attrs.category.as_deref(), Some("staff"));
    }

    /// Purpose: each change file row that does not fit is rejected with its own reason: hiring a
    /// current member, changing or ending someone who is not a member, a change that changes
    /// nothing, absence rows out of sequence, a row with no event, a likely duplicate, a re-key
    /// from an unknown ID, an absence dated before the member joined.
    /// Why it matters: the employer's preview shows rejected rows with reasons so they can be
    /// corrected. A wrong or missing reason sends them hunting through payroll for the wrong thing.
    #[test]
    fn change_file_rows_that_do_not_fit_are_rejected_with_reasons() {
        let f = Fixture::new();
        let mut twin = event(7, "N1", RowChange::Join, None);
        twin.attrs = attrs("staff", 1);
        twin.fingerprint = Some("a".to_string());
        let mut rekey = event(11, "N2", RowChange::Change, None);
        rekey.previous_employee_id = Some("E2".to_string());
        let mut bad_rekey = event(12, "N3", RowChange::Change, None);
        bad_rekey.previous_employee_id = Some("E9".to_string());
        let mut bad_category = event(13, "E3", RowChange::Change, None);
        bad_category.attrs.category = Some("contractor".to_string());
        let mut none = event(8, "E1", RowChange::Join, None);
        none.change = None;
        let rows = vec![
            event(1, "E1", RowChange::Join, None),
            event(2, "E9", RowChange::Change, None),
            event(3, "E9", RowChange::Leave, None),
            event(4, "E1", RowChange::Change, None),
            event(5, "E1", RowChange::AbsenceEnd, None),
            event(6, "E9", RowChange::AbsenceStart, None),
            twin,
            none,
            event(9, "E1", RowChange::AbsenceStart, Some(d(2027, 10, 1))),
            event(10, "E1", RowChange::AbsenceStart, None),
            event(14, "E1", RowChange::AbsenceStart, None),
            rekey,
            bad_rekey,
            bad_category,
        ];
        let out = derive(&f.ctx(), &view(), FileKind::Changes, &rows);
        assert_eq!(
            rejects(&out),
            vec![
                (Some("E1"), "already_member"),
                (Some("E9"), "unknown_member"),
                (Some("E9"), "unknown_member"),
                (Some("E1"), "no_change"),
                (Some("E1"), "not_absent"),
                (Some("E9"), "unknown_member"),
                (Some("N1"), "held_for_review"),
                (Some("E1"), "missing_value"),
                (Some("E1"), "before_joining"),
                (Some("E1"), "already_absent"),
                (Some("N3"), "unknown_previous_id"),
                (Some("E3"), "unknown_value"),
            ]
        );
        assert_eq!(
            out.ambiguities,
            vec![Ambiguity::LikelyDuplicate {
                employee_id: "N1".to_string(),
                other: "E1".to_string()
            }]
        );
        assert_eq!(
            summary(&out),
            vec![("E1".to_string(), "absence_start", d(2027, 12, 1)), ("N2".to_string(), "rekey", d(2027, 12, 1))]
        );
    }

    /// Purpose: the safety check trips only when leavers exceed the scheme's share of the
    /// membership, and never on an empty scheme.
    /// Why it matters: a truncated file looks exactly like mass resignations. Applying one ends
    /// cover for hundreds of people and offers them all continuation; the check is what stops it.
    #[test]
    fn the_safety_check_trips_past_the_share() {
        let t = sample_terms();
        assert!(!trips_safety_check(&t, 100, 20));
        assert!(trips_safety_check(&t, 100, 21));
        assert!(trips_safety_check(&t, 3, 1));
        assert!(!trips_safety_check(&t, 0, 0));
        assert!(!trips_safety_check(&t, 10, 0));
    }
}
