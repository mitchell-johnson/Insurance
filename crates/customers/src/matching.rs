//! Finding whether a person is already known.
//!
//! Candidates are found by blind index: exact on strong identifiers (IRD
//! number, email, mobile) and on phonetic name keys with the date of birth.
//! Each candidate is then scored against what we hold on them, former names
//! included. A link is made automatically only in the high band, which needs
//! a strong identifier as well as agreement on name and date of birth
//! (addendum entry 2): a wrong merge exposes one person's policies to
//! another, so anything less certain is returned as a possible match for a
//! person to decide.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::crypto::IndexKey;
use crate::model::{Field, PartyId};
use crate::party::Party;
use crate::validate::{self, name_key, phonetic};

/// The score at or above which a candidate with a strong identifier in
/// agreement is linked without review.
pub const HIGH: i32 = 80;
/// The score at or above which a candidate is put to a person.
pub const POSSIBLE: i32 = 40;

/// Who to look for. Values are as the caller has them; they are normalised
/// here, and one that is not well formed is set aside rather than failing
/// the whole request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    #[serde(default)]
    pub given_name: Option<String>,
    pub family_name: String,
    #[serde(default)]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub mobile_phone: Option<String>,
    /// An IRD number.
    #[serde(default)]
    pub national_id: Option<String>,
}

/// A probe with every value in normal form, and the names of those set aside.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Normal {
    pub given: Option<String>,
    pub family: String,
    pub dob: Option<NaiveDate>,
    pub email: Option<String>,
    pub mobile: Option<String>,
    pub ird: Option<String>,
    pub ignored: Vec<&'static str>,
}

impl Probe {
    pub fn normal(&self) -> Result<Normal, String> {
        let mut ignored = Vec::new();
        let family = name_key(&self.family_name);
        if family.is_empty() {
            return Err("family_name must contain letters".into());
        }
        let given = self.given_name.as_deref().map(name_key).filter(|g| !g.is_empty());
        let mut keep = |name: &'static str, v: Option<Result<String, String>>| match v {
            Some(Ok(v)) => Some(v),
            Some(Err(_)) => {
                ignored.push(name);
                None
            }
            None => None,
        };
        let email = keep("email", self.email.as_deref().filter(|e| !e.trim().is_empty()).map(validate::email));
        let mobile = keep(
            "mobile_phone",
            self.mobile_phone.as_deref().filter(|e| !e.trim().is_empty()).map(validate::phone),
        );
        let ird = keep("national_id", self.national_id.as_deref().filter(|e| !e.trim().is_empty()).map(validate::ird));
        Ok(Normal {
            given,
            family,
            dob: self.date_of_birth,
            email,
            mobile,
            ird,
            ignored,
        })
    }
}

/// The blind index kinds.
pub mod kind {
    pub const IRD: &str = "ird";
    pub const EMAIL: &str = "email";
    pub const MOBILE: &str = "mobile";
    /// Phonetic family name and date of birth.
    pub const FAMILY_DOB: &str = "family_dob";
    /// Phonetic given name and date of birth: finds a person whose family
    /// name has changed.
    pub const GIVEN_DOB: &str = "given_dob";
    /// Phonetic family and given names: for search without a date of birth.
    pub const NAMES: &str = "names";
    /// Organisation registration number.
    pub const REGISTRATION: &str = "registration";
    /// Phonetic organisation name.
    pub const ORGANISATION: &str = "organisation";
}

/// The index entries a probe looks up.
pub fn probe_keys(n: &Normal, key: &IndexKey) -> Vec<(&'static str, Vec<u8>)> {
    let mut out = Vec::new();
    if let Some(ird) = &n.ird {
        out.push((kind::IRD, key.digest(kind::IRD, ird)));
    }
    if let Some(e) = &n.email {
        out.push((kind::EMAIL, key.digest(kind::EMAIL, e)));
    }
    if let Some(m) = &n.mobile {
        out.push((kind::MOBILE, key.digest(kind::MOBILE, m)));
    }
    if let Some(dob) = n.dob {
        out.push((kind::FAMILY_DOB, key.digest(kind::FAMILY_DOB, &format!("{}|{dob}", phonetic(&n.family)))));
        if let Some(g) = &n.given {
            out.push((kind::GIVEN_DOB, key.digest(kind::GIVEN_DOB, &format!("{}|{dob}", phonetic(g)))));
        }
    } else if let Some(g) = &n.given {
        out.push((kind::NAMES, key.digest(kind::NAMES, &format!("{}|{}", phonetic(&n.family), phonetic(g)))));
    }
    out
}

fn texts(party: &Party, field: Field) -> BTreeSet<String> {
    party
        .assertions
        .iter()
        .filter(|a| a.field == field)
        .filter_map(|a| a.value.as_ref().and_then(Value::as_str).map(str::to_string))
        .collect()
}

/// Every blind index a party should have, over every value it has ever
/// held, so a former name or an old email still finds the person.
pub fn party_keys(party: &Party, key: &IndexKey) -> BTreeSet<(&'static str, Vec<u8>)> {
    let mut out = BTreeSet::new();
    if party.shredded {
        return out;
    }
    for v in texts(party, Field::IrdNumber) {
        out.insert((kind::IRD, key.digest(kind::IRD, &v)));
    }
    for v in texts(party, Field::Email) {
        out.insert((kind::EMAIL, key.digest(kind::EMAIL, &v)));
    }
    for v in texts(party, Field::MobilePhone) {
        out.insert((kind::MOBILE, key.digest(kind::MOBILE, &v)));
    }
    for v in texts(party, Field::RegistrationNumber) {
        out.insert((kind::REGISTRATION, key.digest(kind::REGISTRATION, &v.to_uppercase())));
    }
    for v in texts(party, Field::OrganisationName) {
        out.insert((kind::ORGANISATION, key.digest(kind::ORGANISATION, &phonetic(&v))));
    }
    let families: BTreeSet<String> = texts(party, Field::FamilyName).iter().map(|f| phonetic(f)).collect();
    let givens: BTreeSet<String> = texts(party, Field::GivenName)
        .iter()
        .chain(texts(party, Field::PreferredName).iter())
        .map(|g| phonetic(g))
        .collect();
    let dobs = texts(party, Field::DateOfBirth);
    for dob in &dobs {
        for f in &families {
            out.insert((kind::FAMILY_DOB, key.digest(kind::FAMILY_DOB, &format!("{f}|{dob}"))));
        }
        for g in &givens {
            out.insert((kind::GIVEN_DOB, key.digest(kind::GIVEN_DOB, &format!("{g}|{dob}"))));
        }
    }
    for f in &families {
        for g in &givens {
            out.insert((kind::NAMES, key.digest(kind::NAMES, &format!("{f}|{g}"))));
        }
    }
    out
}

/// How well a candidate agrees with a probe, and whether a strong
/// identifier agrees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Score {
    pub party: PartyId,
    pub score: i32,
    pub strong: bool,
}

fn best_similarity(probe: &str, held: &BTreeSet<String>) -> f64 {
    held.iter().map(|h| strsim::jaro_winkler(probe, &name_key(h))).fold(0.0, f64::max)
}

/// Score one candidate. A strong identifier that disagrees, or a different
/// date of birth, counts heavily against: those are different people.
pub fn score(n: &Normal, party: &Party) -> Score {
    let mut score = 0;
    let mut strong = false;
    let mut exact = |probe: &Option<String>, field: Field, agree: i32, disagree: i32| {
        if let Some(p) = probe {
            let held = texts(party, field);
            if held.contains(p) {
                score += agree;
                strong = true;
            } else if !held.is_empty() {
                score -= disagree;
            }
        }
    };
    exact(&n.ird, Field::IrdNumber, 60, 100);
    // People change email and phone; a different one says little.
    exact(&n.email, Field::Email, 20, 0);
    exact(&n.mobile, Field::MobilePhone, 15, 0);
    if let Some(dob) = n.dob {
        let held = texts(party, Field::DateOfBirth);
        if held.contains(&dob.to_string()) {
            score += 25;
        } else if !held.is_empty() {
            score -= 40;
        }
    }
    let family = best_similarity(&n.family, &texts(party, Field::FamilyName));
    score += if family >= 0.97 {
        20
    } else if family >= 0.90 {
        12
    } else {
        0
    };
    if let Some(g) = &n.given {
        let mut held = texts(party, Field::GivenName);
        held.extend(texts(party, Field::PreferredName));
        let given = best_similarity(g, &held);
        let initial = held.iter().any(|h| {
            let h = name_key(h);
            (h.len() == 1 || g.len() == 1) && h.chars().next() == g.chars().next()
        });
        score += if given >= 0.95 {
            15
        } else if given >= 0.88 {
            8
        } else if initial {
            5
        } else {
            0
        };
    }
    Score {
        party: party.id.clone(),
        score,
        strong,
    }
}

/// What a match concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Resolution {
    /// Exactly one high-band candidate: the probe is that person.
    Linked { party: PartyId },
    /// No candidate: a new person was created.
    Created { party: PartyId },
    /// People the probe might be. A person decides.
    Possible { candidates: Vec<PartyId> },
    /// No candidate, and the caller did not ask for one to be created.
    None,
}

/// Decide from the scored candidates. `None` means nobody is close enough
/// and the caller may create the person.
pub fn conclude(mut scores: Vec<Score>) -> Option<Resolution> {
    scores.retain(|s| s.score >= POSSIBLE);
    scores.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.party.cmp(&b.party)));
    if scores.is_empty() {
        return None;
    }
    let high: Vec<&Score> = scores.iter().filter(|s| s.score >= HIGH && s.strong).collect();
    if high.len() == 1 {
        return Some(Resolution::Linked { party: high[0].party.clone() });
    }
    Some(Resolution::Possible {
        candidates: scores.into_iter().map(|s| s.party).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventBody, Recorded};
    use crate::model::{Assertion, PartyKind};
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    pub(crate) fn person(id: &str, facts: &[(Field, &str)]) -> Party {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let mut p = Party::new(PartyId(id.into()));
        p.apply(&Recorded {
            seq: 1,
            recorded_at: at,
            caller: "t".into(),
            actor: "t".into(),
            body: EventBody::Created {
                kind: PartyKind::Person,
                group: "g".into(),
                source: "t".into(),
            },
        });
        let assertions = facts
            .iter()
            .map(|(f, v)| Assertion {
                field: *f,
                value: Some(json!(v)),
                valid_from: at.date_naive(),
                recorded_at: at,
                reason: None,
                source: "t".into(),
            })
            .collect();
        p.apply(&Recorded {
            seq: 2,
            recorded_at: at,
            caller: "t".into(),
            actor: "t".into(),
            body: EventBody::Asserted { assertions },
        });
        p
    }

    fn probe(given: &str, family: &str, dob: Option<&str>) -> Normal {
        Probe {
            given_name: Some(given.into()),
            family_name: family.into(),
            date_of_birth: dob.map(|d| d.parse().unwrap()),
            ..Probe::default()
        }
        .normal()
        .unwrap()
    }

    /// Purpose: name and date of birth alone, however exact, never reach the high band;
    /// with an agreeing IRD number they do.
    /// Why it matters: addendum entry 2 says only a confident match links automatically;
    /// two people with the same name and birthday exist, and a wrong link shows one
    /// person's policies to another.
    #[test]
    fn name_and_birth_date_alone_are_only_possible() {
        let held = person(
            "p1",
            &[
                (Field::GivenName, "Aroha"),
                (Field::FamilyName, "Ngata"),
                (Field::DateOfBirth, "1980-02-03"),
                (Field::IrdNumber, "49091850"),
            ],
        );
        let n = probe("Aroha", "Ngata", Some("1980-02-03"));
        let s = score(&n, &held);
        assert!(s.score >= POSSIBLE && !s.strong, "{s:?}");
        assert_eq!(
            conclude(vec![s]),
            Some(Resolution::Possible {
                candidates: vec![PartyId("p1".into())]
            })
        );
        let mut n = n;
        n.ird = Some("49091850".into());
        let s = score(&n, &held);
        assert!(s.score >= HIGH && s.strong);
        assert_eq!(conclude(vec![s]), Some(Resolution::Linked { party: PartyId("p1".into()) }));
    }

    /// Purpose: a disagreeing IRD number or date of birth pushes a candidate out of range,
    /// and weak agreement on spelling variants still scores.
    /// Why it matters: strong contrary evidence must override name similarity, while a
    /// "Jon"/"John" typo should still surface the existing customer for review.
    #[test]
    fn contrary_evidence_outweighs_similar_names() {
        let held = person(
            "p1",
            &[
                (Field::GivenName, "John"),
                (Field::FamilyName, "Smith"),
                (Field::DateOfBirth, "1975-05-06"),
                (Field::IrdNumber, "49091850"),
                (Field::Email, "j@x.nz"),
                (Field::MobilePhone, "+64211234567"),
            ],
        );
        let mut n = probe("John", "Smith", Some("1975-05-06"));
        n.ird = Some("35901981".into());
        assert!(score(&n, &held).score < POSSIBLE);
        let n = probe("John", "Smith", Some("1975-05-07"));
        assert!(score(&n, &held).score < POSSIBLE);
        let mut n = probe("Jon", "Smyth", Some("1975-05-06"));
        n.email = Some("other@x.nz".into());
        n.mobile = Some("+64211234567".into());
        let s = score(&n, &held);
        assert!(s.strong && s.score >= POSSIBLE, "{s:?}");
        let n = probe("J", "Smith", None);
        assert_eq!(score(&n, &held).score, 25);
        let n = probe("Mary", "Brown", None);
        assert_eq!(score(&n, &held).score, 0);
        let n = probe("Johnny", "Smithe", None);
        assert!(score(&n, &held).score > 0);
    }

    /// Purpose: two high-band candidates are returned as possible, ordered by score, and
    /// candidates below the possible band are dropped.
    /// Why it matters: when two records both look certain, the system must not pick one;
    /// that is exactly the duplicate a person has to resolve.
    #[test]
    fn ambiguity_goes_to_a_person() {
        let s = |p: &str, score: i32, strong: bool| Score {
            party: PartyId(p.into()),
            score,
            strong,
        };
        assert_eq!(conclude(vec![s("a", 10, true)]), None);
        assert_eq!(
            conclude(vec![s("a", 90, true), s("b", 95, true), s("c", 20, false)]),
            Some(Resolution::Possible {
                candidates: vec![PartyId("b".into()), PartyId("a".into())]
            })
        );
    }

    /// Purpose: probes normalise their values, set aside malformed identifiers, and look up
    /// the index entries the party side writes for the same person, including after a
    /// change of family name.
    /// Why it matters: if the two sides computed index keys differently, matching would
    /// silently find nobody and every joiner would be created as a new person.
    #[test]
    fn probe_and_party_keys_agree() {
        let key = IndexKey::new(&[3; 32]).unwrap();
        let mut held = person(
            "p1",
            &[
                (Field::GivenName, "Mere"),
                (Field::FamilyName, "Smith"),
                (Field::DateOfBirth, "1990-01-01"),
                (Field::Email, "mere@x.nz"),
                (Field::MobilePhone, "+64211234567"),
                (Field::IrdNumber, "49091850"),
            ],
        );
        let keys = party_keys(&held, &key);
        let p = Probe {
            given_name: Some("Mere".into()),
            family_name: "Smyth".into(),
            date_of_birth: Some("1990-01-01".parse().unwrap()),
            email: Some(" MERE@x.nz".into()),
            mobile_phone: Some("+64 21 123 4567".into()),
            national_id: Some("49-091-850".into()),
        };
        let n = p.normal().unwrap();
        for k in probe_keys(&n, &key) {
            assert!(keys.contains(&k), "{}", k.0);
        }
        // Married: a new family name, found by given name and birth date.
        let n = probe("Mere", "Walker", Some("1990-01-01"));
        assert!(probe_keys(&n, &key).iter().any(|k| keys.contains(k)));
        // Without a birth date, the names key.
        let n = probe("Mere", "Smith", None);
        assert!(probe_keys(&n, &key).iter().all(|k| keys.contains(k)));
        // Malformed identifiers are set aside, not fatal.
        let bad = Probe {
            family_name: "Smith".into(),
            email: Some("nope".into()),
            mobile_phone: Some("123".into()),
            national_id: Some("1".into()),
            ..Probe::default()
        }
        .normal()
        .unwrap();
        assert_eq!(bad.ignored, vec!["email", "mobile_phone", "national_id"]);
        assert!(Probe {
            family_name: "--".into(),
            ..Probe::default()
        }
        .normal()
        .is_err());
        held.shredded = true;
        assert!(party_keys(&held, &key).is_empty());
    }
}
