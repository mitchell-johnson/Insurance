//! The retention schedule, and the register of erasures.
//!
//! The schedule is code: each class of data is kept for a period after the
//! party's last relationship ends, and a party is erased once everything
//! held on them has expired, no legal hold stands and nothing is in force.
//! The periods need legal review before go-live (see `docs/customers.md`).
//!
//! Erasure destroys the party's wrapped key in the database. A database
//! restored from a backup taken before an erasure brings the key back, so
//! every erasure is first written to a register in write-once Blob Storage
//! that a restore cannot roll back, and the service erases again everything
//! the register lists since the backup was taken.

use chrono::{DateTime, Months, NaiveDate, Utc};
use ic_azure::blob::WormStore;
use ic_azure::AzureError;
use serde::{Deserialize, Serialize};

use crate::model::{FieldClass, Lifecycle, PartyId};
use crate::party::Party;

/// The version of the schedule below. Recorded with every erasure.
pub const SCHEDULE_VERSION: &str = "2026-11-12";

/// Years each class is kept after the last relationship ends.
pub fn years(class: FieldClass) -> u32 {
    match class {
        // Tax records: seven years (Tax Administration Act 1994, s 22).
        FieldClass::Tax => 7,
        // Identity, contact and organisation details support claims and
        // complaints raised after a policy ends: seven years.
        FieldClass::Identity | FieldClass::Contact | FieldClass::Organisation | FieldClass::Restricted => 7,
    }
}

/// Identity verification records: five years after the relationship ends
/// (AML/CFT Act 2009, s 50).
pub const VERIFICATION_YEARS: u32 = 5;
/// A prospect who never had a relationship: two years after last contact.
pub const PROSPECT_YEARS: u32 = 2;

fn plus_years(day: NaiveDate, years: u32) -> NaiveDate {
    day.checked_add_months(Months::new(years * 12)).unwrap_or(NaiveDate::MAX)
}

/// The first day the party may be erased, or `None` while something keeps
/// it: a hold, a merge into another party, or a relationship in force or
/// still to start.
pub fn erasable_from(party: &Party, last_change: DateTime<Utc>, today: NaiveDate) -> Option<NaiveDate> {
    if !party.exists() || party.shredded || !party.holds.is_empty() || party.merged_into.is_some() {
        return None;
    }
    if party.relationships.values().any(|r| r.valid_to.is_none_or(|end| end > today)) {
        return None;
    }
    let last_end = party.relationships.values().filter_map(|r| r.valid_to).max();
    let Some(ended) = last_end else {
        if party.lifecycle == Some(Lifecycle::Active) {
            // Activated by hand with no relationship: kept as a customer.
            return Some(plus_years(last_change.date_naive(), 7));
        }
        return Some(plus_years(last_change.date_naive(), PROSPECT_YEARS));
    };
    let mut keep = party.assertions.iter().map(|a| years(a.field.class())).max().unwrap_or(0);
    if !party.verifications.is_empty() {
        keep = keep.max(VERIFICATION_YEARS);
    }
    Some(plus_years(ended.max(last_change.date_naive()), keep))
}

/// One page of the erasure register.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub schedule: String,
    pub at: DateTime<Utc>,
    pub parties: Vec<PartyId>,
}

/// The erasure register: pages named `erased/<date>/<n>.json`, numbered
/// from 1 each day, written once and never changed.
pub struct Register<S> {
    store: S,
}

fn page_name(day: NaiveDate, n: u32) -> String {
    format!("erased/{day}/{n:06}.json")
}

impl<S: WormStore> Register<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Write a page listing parties about to be erased.
    pub async fn record(&self, parties: &[PartyId], at: DateTime<Utc>) -> Result<String, String> {
        let page = Page {
            schedule: SCHEDULE_VERSION.to_string(),
            at,
            parties: parties.to_vec(),
        };
        let body = serde_json::to_vec(&page).map_err(|e| e.to_string())?;
        let day = at.date_naive();
        for n in 1..=100_000 {
            let name = page_name(day, n);
            match self.store.put_new(&name, "application/json", body.clone()).await {
                Ok(()) => return Ok(name),
                Err(AzureError::AlreadyExists(_)) => continue,
                Err(e) => return Err(format!("the erasure register could not be written: {e}")),
            }
        }
        Err("the erasure register has no free page today".into())
    }

    /// Every party listed on or after `since`.
    pub async fn since(&self, since: NaiveDate, today: NaiveDate) -> Result<Vec<PartyId>, String> {
        let mut out = Vec::new();
        let mut day = since;
        while day <= today {
            for n in 1.. {
                let body = self
                    .store
                    .get(&page_name(day, n))
                    .await
                    .map_err(|e| format!("the erasure register could not be read: {e}"))?;
                let Some(body) = body else { break };
                let page: Page = serde_json::from_slice(&body).map_err(|e| format!("erasure register page is unreadable: {e}"))?;
                out.extend(page.parties);
            }
            day = day.succ_opt().unwrap_or(NaiveDate::MAX);
            if day == NaiveDate::MAX {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventBody, Recorded};
    use crate::model::{Field, PartyKind, Relationship, RelationshipKind, Target, Verification, VerificationOutcome};
    use chrono::TimeZone;
    use ic_azure::blob::MemoryWormStore;
    use serde_json::json;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn base() -> (Party, DateTime<Utc>) {
        let at = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let mut p = Party::new(PartyId("p".into()));
        let mut seq = 0;
        let mut add = |p: &mut Party, body| {
            seq += 1;
            p.apply(&Recorded {
                seq,
                recorded_at: at,
                caller: "t".into(),
                actor: "t".into(),
                body,
            })
        };
        add(
            &mut p,
            EventBody::Created {
                kind: PartyKind::Person,
                group: "g".into(),
                source: "t".into(),
            },
        );
        add(
            &mut p,
            EventBody::Asserted {
                assertions: vec![crate::model::Assertion {
                    field: Field::FamilyName,
                    value: Some(json!("Ngata")),
                    valid_from: at.date_naive(),
                    valid_to: None,
                    recorded_at: at,
                    reason: None,
                    source: "t".into(),
                }],
            },
        );
        (p, at)
    }

    fn relationship(end: Option<&str>) -> Relationship {
        Relationship {
            id: "r1".into(),
            kind: RelationshipKind::Owner,
            target: Target::Policy("POL1".into()),
            valid_from: d("2020-01-01"),
            valid_to: end.map(d),
            recorded_at: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
            attributes: Default::default(),
        }
    }

    /// Purpose: a customer whose last policy ended is kept for the longest period of the
    /// classes held, a prospect for two years, and nobody while a relationship, a hold or
    /// a merge stands.
    /// Why it matters: erasing too early destroys records the law or a claim needs;
    /// keeping too long breaches the privacy principle of not keeping data longer than
    /// needed.
    #[test]
    fn the_schedule_sets_the_erasure_date() {
        let (mut p, at) = base();
        let today = d("2026-01-01");
        assert_eq!(erasable_from(&p, at, today), Some(d("2022-01-01")));
        p.lifecycle = Some(Lifecycle::Active);
        assert_eq!(erasable_from(&p, at, today), Some(d("2027-01-01")));
        p.relationships.insert("r1".into(), relationship(None));
        assert_eq!(erasable_from(&p, at, today), None);
        p.relationships.insert("r1".into(), relationship(Some("2027-01-01")));
        assert_eq!(erasable_from(&p, at, today), None);
        p.relationships.insert("r1".into(), relationship(Some("2021-06-30")));
        assert_eq!(erasable_from(&p, at, today), Some(d("2028-06-30")));
        p.assertions.clear();
        assert_eq!(erasable_from(&p, at, today), Some(d("2021-06-30")));
        p.verifications.push(Verification {
            method: "document".into(),
            provider: "p".into(),
            outcome: VerificationOutcome::Passed,
            evidence_ref: "e".into(),
            verified_at: at,
        });
        assert_eq!(erasable_from(&p, at, today), Some(d("2026-06-30")));
        p.holds.insert("LH1".into());
        assert_eq!(erasable_from(&p, at, today), None);
        p.holds.clear();
        p.merged_into = Some(PartyId("q".into()));
        assert_eq!(erasable_from(&p, at, today), None);
        assert_eq!(erasable_from(&Party::default(), at, today), None);
        assert_eq!(plus_years(NaiveDate::MAX, 1), NaiveDate::MAX);
    }

    /// Purpose: register pages are numbered per day without overwriting, and reading from
    /// a date returns every party listed since, across days.
    /// Why it matters: after a database restore, this list is the only thing that stops an
    /// erased person's key from coming back to life.
    #[tokio::test]
    async fn the_register_lists_every_erasure() {
        let reg = Register::new(MemoryWormStore::default());
        let day1 = Utc.with_ymd_and_hms(2026, 3, 1, 1, 0, 0).unwrap();
        let day2 = Utc.with_ymd_and_hms(2026, 3, 3, 1, 0, 0).unwrap();
        assert_eq!(reg.record(&[PartyId("a".into())], day1).await.unwrap(), "erased/2026-03-01/000001.json");
        assert_eq!(reg.record(&[PartyId("b".into())], day1).await.unwrap(), "erased/2026-03-01/000002.json");
        reg.record(&[PartyId("c".into())], day2).await.unwrap();
        let all = reg.since(d("2026-03-01"), d("2026-03-05")).await.unwrap();
        assert_eq!(all, vec![PartyId("a".into()), PartyId("b".into()), PartyId("c".into())]);
        assert_eq!(reg.since(d("2026-03-02"), d("2026-03-05")).await.unwrap(), vec![PartyId("c".into())]);
        assert!(reg.since(NaiveDate::MAX, NaiveDate::MAX).await.unwrap().is_empty());
    }
}
