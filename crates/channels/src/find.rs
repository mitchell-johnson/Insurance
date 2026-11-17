//! Finding a customer or a policy from what a member of staff knows: a
//! name, a birth date, an email, a phone number, an IRD number or a policy
//! number, typed into one box. Nobody should have to know a party's or a
//! policy's internal reference to start work.
//!
//! The words are read here into a probe for the customer master's own
//! search (which scores every person it holds) and a policy number. The
//! master answers with references only; the people are then read one at a
//! time, each read asked of the four questions and logged like any other,
//! and joined to the policies they own or are insured on from the channel
//! state's directory. Nothing here decides access.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use ic_core::{PartyId, PolicyId};
use ic_readmodel::Projections;
use serde_json::{json, Map, Value};

use crate::directory::Directory;

/// The most people read for one search. The master scores everyone; only
/// the best few are worth a person's time, and each read is logged.
pub const PEOPLE: usize = 8;
/// The most policies listed for a number typed in part.
pub const POLICIES: usize = 10;
/// The fewest characters searched on. One letter finds half the book.
pub const SHORTEST: usize = 2;

/// What one search box held, read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Wanted {
    /// The text as a policy number (or the start of one), when it is one word.
    pub number: Option<String>,
    /// A probe for the customer master's search, when there is a family name.
    pub probe: Option<Value>,
    /// Why nobody can be searched for, where something else was typed.
    pub hint: Option<&'static str>,
}

/// A birth date as people write it: 1984-03-07, 7/3/1984 or 7.3.1984 (day
/// first, as in New Zealand), with the year in full or as two digits. Two
/// digits are a year this century up to this year, else last century: a
/// person is born in the past, so 84 is 1984 and 05 is 2005.
fn date(word: &str) -> Option<NaiveDate> {
    if let Ok(d) = NaiveDate::parse_from_str(word, "%Y-%m-%d") {
        return Some(d);
    }
    let parts: Vec<&str> = word.split(['/', '.', '-']).collect();
    let [day, month, year] = parts[..] else { return None };
    let year: i32 = match year.len() {
        4 => year.parse().ok()?,
        2 => {
            let short: i32 = year.parse().ok()?;
            let now = chrono::Datelike::year(&chrono::Utc::now().date_naive()) % 100;
            if short <= now {
                2000 + short
            } else {
                1900 + short
            }
        }
        _ => return None,
    };
    NaiveDate::from_ymd_opt(year, month.parse().ok()?, day.parse().ok()?)
}

fn is_name(word: &str) -> bool {
    word.chars().any(char::is_alphabetic) && word.chars().all(|c| c.is_alphabetic() || c == '-' || c == '\'' || c == '’')
}

/// Read what was typed. A comma after the first words means "family name,
/// given names" (Ngata, Kiri); otherwise the last name word is the family
/// name (Kiri Ngata). Digits are an IRD number (8 or 9, not starting 0) or
/// a phone number (starting 0 or +); a word with @ is an email.
pub fn read(text: &str) -> Result<Wanted, String> {
    let text = text.trim();
    if text.chars().count() < SHORTEST {
        return Err(format!("type at least {SHORTEST} characters to search"));
    }
    let one_word = !text.contains(char::is_whitespace) && !text.contains(',');
    let mut wanted = Wanted {
        number: one_word.then(|| text.to_string()),
        ..Wanted::default()
    };
    let (family_first, rest) = match text.split_once(',') {
        Some((head, tail)) if head.split_whitespace().all(is_name) && !head.trim().is_empty() => (Some(head.trim()), tail),
        _ => (None, text),
    };
    let mut probe = Map::new();
    let mut names = Vec::new();
    for word in rest.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()) {
        if let Some(d) = date(word) {
            probe.insert("date_of_birth".into(), json!(d));
        } else if word.contains('@') {
            probe.insert("email".into(), json!(word));
        } else if word.starts_with('+') || (word.starts_with('0') && word.chars().filter(char::is_ascii_digit).count() >= 8) {
            probe.insert("mobile_phone".into(), json!(word));
        } else if word.chars().all(|c| c.is_ascii_digit() || c == '-') && (8..=9).contains(&word.chars().filter(char::is_ascii_digit).count()) {
            probe.insert("national_id".into(), json!(word.replace('-', "")));
        } else if is_name(word) {
            names.push(word);
        }
    }
    let family = match family_first {
        Some(f) => Some(f.to_string()),
        None => names.pop().map(str::to_string),
    };
    match family {
        Some(family) => {
            probe.insert("family_name".into(), json!(family));
            if !names.is_empty() {
                probe.insert("given_name".into(), json!(names.join(" ")));
            }
            wanted.probe = Some(Value::Object(probe));
        }
        None if !probe.is_empty() => wanted.hint = Some("add the person's family name to search the customer master"),
        None => {}
    }
    // A family name alone is shared by many people, and the master scores it too low to list anyone.
    if wanted.probe.as_ref().is_some_and(|p| p.as_object().is_some_and(|o| o.len() == 1)) && !one_word {
        wanted.hint = Some("a family name alone fits too many people: add a given name or the birth date");
    }
    Ok(wanted)
}

/// One policy as a search lists it: references, product and standing only.
fn entry(view: &Projections, policy: &PolicyId, role: &str) -> Option<Value> {
    let e = view.policy(policy)?;
    Some(json!({ "policy": e.policy, "number": e.number, "product": e.product_code, "status": e.status, "partition": e.partition, "as": role }))
}

/// The fewest characters of the end of a policy number searched on: a
/// caller often reads out only its last few.
pub const SHORTEST_ENDING: usize = 3;

/// Policies whose number (or reference) is the text, starts with it or
/// (from three characters) ends with it, best first.
pub fn numbered(view: &Projections, text: &str) -> Vec<Value> {
    let want = text.to_lowercase();
    let mut exact = Vec::new();
    let mut starting = Vec::new();
    let mut ending = Vec::new();
    for e in view.directory() {
        let (number, id) = (e.number.to_lowercase(), e.policy.0.to_lowercase());
        if number == want || id == want {
            exact.push(e);
        } else if number.starts_with(&want) || id.starts_with(&want) {
            starting.push(e);
        } else if want.chars().count() >= SHORTEST_ENDING && number.ends_with(&want) {
            ending.push(e);
        }
    }
    starting.sort_by(|a, b| a.number.cmp(&b.number));
    ending.sort_by(|a, b| a.number.cmp(&b.number));
    exact
        .into_iter()
        .chain(starting)
        .chain(ending)
        .take(POLICIES)
        .filter_map(|e| entry(view, &e.policy, "number"))
        .collect()
}

/// The policies each person owns or is insured on, owned first.
pub fn policies_of(view: &Projections, directory: &Directory, parties: &[PartyId]) -> BTreeMap<PartyId, Vec<Value>> {
    let wanted: BTreeSet<&PartyId> = parties.iter().collect();
    let mut out: BTreeMap<PartyId, Vec<Value>> = parties.iter().map(|p| (p.clone(), Vec::new())).collect();
    let mut seen: BTreeSet<(PartyId, PolicyId)> = BTreeSet::new();
    for (policy, owners) in &directory.policyholders {
        for owner in owners.iter().filter(|o| wanted.contains(o)) {
            if let Some(row) = entry(view, policy, "owner") {
                seen.insert((owner.clone(), policy.clone()));
                out.entry(owner.clone()).or_default().push(row);
            }
        }
    }
    for party in parties {
        for layer in view.layers_on_party(party) {
            if seen.insert((party.clone(), layer.policy.clone())) {
                if let Some(row) = entry(view, &layer.policy, "life insured") {
                    out.entry(party.clone()).or_default().push(row);
                }
            }
        }
    }
    out
}

/// A person as a search lists them, from the master's answer to a read:
/// the name they go by and their birth date, nothing else.
pub fn person(party: &PartyId, record: &Value) -> Value {
    let fact = |f: &str| {
        let v = &record["facts"][f];
        v.get("value").unwrap_or(v).as_str().map(str::to_string)
    };
    let given = fact("preferred_name").or_else(|| fact("given_name"));
    let name = [given, fact("family_name")].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let name = if name.is_empty() {
        fact("organisation_name").unwrap_or_default()
    } else {
        name
    };
    json!({ "party": party, "name": name, "born": fact("date_of_birth"), "died": fact("date_of_death") })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Purpose: the words a member of staff types are read as the master's probe: last word the family name, a comma for family first, dates day first, digits as an IRD or phone number, an @ as an email.
    /// Why it matters: a caller gives their name and birth date in whatever order they say it; read wrongly, the search finds nobody and the person is back to asking for a reference the caller never had. A year said as two digits, and a family name alone, were both met with silence (found by the scenario run, SCN-13).
    #[test]
    fn typed_words_become_a_probe() {
        let p = |t: &str| read(t).unwrap().probe.unwrap();
        assert_eq!(p("Kiri Ngata"), json!({ "family_name": "Ngata", "given_name": "Kiri" }));
        assert_eq!(p("Ngata, Kiri Anne"), json!({ "family_name": "Ngata", "given_name": "Kiri Anne" }));
        assert_eq!(
            p("kiri ngata 7/3/1984"),
            json!({ "family_name": "ngata", "given_name": "kiri", "date_of_birth": "1984-03-07" })
        );
        assert_eq!(p("Ngata 1984-03-07")["date_of_birth"], "1984-03-07");
        assert_eq!(p("Ngata 07.03.1984")["date_of_birth"], "1984-03-07");
        assert_eq!(p("Ngata 07-03-1984")["date_of_birth"], "1984-03-07");
        // A two-digit year is the last time it came round (found by the scenario run, SCN-13).
        assert_eq!(p("Ngata 7/3/84")["date_of_birth"], "1984-03-07");
        assert_eq!(p("Ngata 07/03/05")["date_of_birth"], "2005-03-07");
        assert_eq!(p("Ngata 31/2/1984").get("date_of_birth"), None, "no such day");
        assert_eq!(p("Ngata 7/3/984").get("date_of_birth"), None, "a three-digit year is no year");
        assert_eq!(p("Ngata 7/3").get("date_of_birth"), None);
        assert_eq!(p("Ngata 7/x/84").get("date_of_birth"), None);
        assert_eq!(p("Ngata 7/3/x4").get("date_of_birth"), None);
        assert_eq!(p("Ngata 7/3/x984").get("date_of_birth"), None);
        // A family name alone is told to need more; one word is also tried as a policy number, so it is not.
        assert_eq!(
            read("Kiri, ").unwrap().hint,
            Some("a family name alone fits too many people: add a given name or the birth date")
        );
        assert_eq!(read("Ngata").unwrap().hint, None);
        assert_eq!(p("O'Brien-Smith k@x.nz")["email"], "k@x.nz");
        assert_eq!(p("Ngata 021 555 1234")["family_name"], "Ngata");
        assert_eq!(p("Ngata 0215551234")["mobile_phone"], "0215551234");
        assert_eq!(p("Ngata +64215551234")["mobile_phone"], "+64215551234");
        assert_eq!(p("Ngata 123-456-789")["national_id"], "123456789");
        let number = read("POL-0042").unwrap();
        assert_eq!(number.number.as_deref(), Some("POL-0042"));
        assert_eq!(number.probe, None, "a policy number is not a name");
        assert_eq!(read("ngata").unwrap().number.as_deref(), Some("ngata"), "one word is tried as both");
        assert_eq!(read("Kiri Ngata").unwrap().number, None);
        assert_eq!(
            read("7/3/1984 k@x.nz").unwrap().hint,
            Some("add the person's family name to search the customer master")
        );
        assert!(read(" k ").unwrap_err().contains("at least 2"));
    }

    /// Purpose: a person is listed by the name they go by and their birth date, an organisation by its name, from the master's facts with or without their dates.
    /// Why it matters: a list of party references is no use on a call; staff pick the right person by name and birth date, and never see more of the record than that until they open it.
    #[test]
    fn a_person_is_listed_by_name_and_birth() {
        let kiri = person(
            &PartyId::from("p-1"),
            &json!({ "facts": { "given_name": { "value": "Kiri" }, "preferred_name": { "value": "Kiri-Ann" }, "family_name": { "value": "Ngata" }, "date_of_birth": { "value": "1984-03-07" }, "email": { "value": "k@x.nz" } } }),
        );
        assert_eq!(kiri, json!({ "party": "p-1", "name": "Kiri-Ann Ngata", "born": "1984-03-07", "died": null }));
        let firm = person(&PartyId::from("p-2"), &json!({ "facts": { "organisation_name": "Tane Builders" } }));
        assert_eq!((firm["name"].as_str(), firm["born"].is_null()), (Some("Tane Builders"), true));
        assert_eq!(person(&PartyId::from("p-3"), &json!({}))["name"], "");
    }
}
