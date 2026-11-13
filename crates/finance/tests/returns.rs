//! Regulatory returns with lineage, restatements, and conduct measures.

mod common;

use common::*;
use ic_finance::books::{FinEvent, JournalKind};
use ic_finance::config::ConfigError;
use ic_finance::returns::{ConductFact, ConductMeasure, ReturnDefinition, MAX_CONDUCT_FACTS};
use ic_finance::{FinCommand, FinanceState};
use ic_money::FINE_PER_MINOR;

const QUARTERLY: &str = include_str!("../../../config/finance/returns/insurer_quarterly.star");

fn definition() -> ReturnDefinition {
    ReturnDefinition::load("insurer_quarterly.star", QUARTERLY).unwrap()
}

fn fact(id: &str, measure: ConductMeasure, date: chrono::NaiveDate) -> ConductFact {
    ConductFact {
        id: id.to_string(),
        measure,
        date,
        product: Some("term-life".to_string()),
        relates_to: None,
    }
}

fn undoing(id: &str, measure: ConductMeasure, date: chrono::NaiveDate, earlier: chrono::NaiveDate) -> ConductFact {
    ConductFact {
        relates_to: Some(earlier),
        ..fact(id, measure, date)
    }
}

fn record(facts: Vec<ConductFact>) -> FinCommand {
    FinCommand::RecordConductFacts { facts }
}

/// A core run from 20 November 2027 to `until` with one paying policy.
fn world(until: chrono::NaiveDate) -> World {
    let mut w = World::new(1, d(2027, 11, 20));
    let (p, id) = w.issue(1, d(2027, 11, 20));
    w.pay(p, &id, "r1", 3 * MONTHLY);
    w.until(until);
    w
}

/// Purpose: a decline that is later overturned stays counted as a decline in the month it
/// was made. The overturn is counted on its own in the month it happened, against the month
/// of the decline, and a reversed admission is counted the same way. A fact sent twice is
/// counted once, and a fact from the wrong service, malformed, or dated in a closed period
/// is refused with nothing counted. A replacement written is counted only when the
/// distribution module reports it.
/// Why it matters: this is the design addendum's fifth decision. Netting overturns against
/// declines would make the decline rate look better exactly when the insurer got more
/// decisions wrong, which is the opposite of what a conduct regulator needs to see.
#[test]
fn an_overturned_decline_is_reported_apart_from_the_declines() {
    use ConductMeasure::*;
    let mut w = world(d(2028, 1, 20));
    let (nov, dec, jan) = (period(2027, 11), period(2027, 12), period(2028, 1));
    let b = &mut w.books;
    let december = vec![
        fact("clm-1", ClaimAccepted, d(2027, 12, 3)),
        fact("clm-2", ClaimAccepted, d(2027, 12, 9)),
        fact("clm-3", ClaimDeclined, d(2027, 12, 10)),
        fact("clm-4", ClaimDeclined, d(2027, 12, 15)),
        ConductFact {
            product: Some("health-plan".to_string()),
            ..fact("clm-5", ClaimDeclined, d(2027, 12, 20))
        },
    ];
    let first = b.send_keyed("dec-facts", "claims", None, record(december.clone()));
    assert!(matches!(first.events[0].event, FinEvent::ConductFactsRecorded { count: 5 }));
    // Sent again with one new fact: only the new one counts.
    let mut resent = december;
    resent.push(fact("clm-6", ClaimDeclined, d(2028, 1, 5)));
    let second = b.send_keyed("resend", "claims", None, record(resent.clone()));
    assert!(matches!(second.events[0].event, FinEvent::ConductFactsRecorded { count: 1 }));
    let third = b.send_keyed("resend-2", "claims", None, record(resent));
    assert_eq!((third.result, third.events.len()), (Ok(()), 0));

    let january = vec![
        undoing("clm-3/overturned", DeclineOverturned, d(2028, 1, 12), d(2027, 12, 10)),
        undoing("clm-1/reversed", AdmissionReversed, d(2028, 1, 14), d(2027, 12, 3)),
    ];
    b.send("claims", None, record(january)).unwrap();
    let complaints = vec![
        fact("cmp-1", ComplaintReceived, d(2028, 1, 6)),
        fact("cmp-2", ComplaintReceived, d(2028, 1, 8)),
        fact("cmp-1/upheld", ComplaintUpheld, d(2028, 1, 15)),
    ];
    b.send("complaints", None, record(complaints)).unwrap();
    let replaced = vec![fact("layer-9/replacement", ReplacementWritten, d(2028, 1, 7))];
    assert_eq!(b.code("claims", None, record(replaced.clone())), "caller");
    b.send("distribution", None, record(replaced)).unwrap();

    let was = b.state.conduct_report(dec, None);
    assert_eq!((was.accepted, was.declined), (2, 3), "December's declines are still three");
    assert_eq!(was.declines_later_overturned, 1);
    assert!(was.overturned.is_empty() && was.admissions_reversed.is_empty());
    let now = b.state.conduct_report(jan, None);
    assert_eq!((now.declined, now.accepted), (1, 0));
    assert_eq!(now.overturned.iter().collect::<Vec<_>>(), vec![(&dec, &1)]);
    assert_eq!(now.admissions_reversed.iter().collect::<Vec<_>>(), vec![(&dec, &1)]);
    assert_eq!((now.complaints_received, now.complaints_upheld), (2, 1));
    assert_eq!((now.replacements, was.replacements), (1, 0));
    assert_eq!(b.state.conduct_report(dec, Some("health-plan")).declined, 1);
    assert_eq!(b.state.conduct_report(dec, Some("term-life")).declined, 2);

    // A refused batch counts none of its facts, the good one included.
    let counted = b.state.conduct_report(jan, None);
    let refused = |b: &mut Books, caller: &str, bad: ConductFact| b.code(caller, None, record(vec![fact("good", ClaimAccepted, d(2028, 1, 2)), bad]));
    assert_eq!(refused(b, "complaints", fact("a", ClaimDeclined, d(2028, 1, 2))), "caller");
    assert_eq!(refused(b, "claims", fact("a", ComplaintReceived, d(2028, 1, 2))), "caller");
    assert_eq!(b.code("payments", None, record(vec![])), "caller");
    assert_eq!(refused(b, "claims", fact("", ClaimDeclined, d(2028, 1, 2))), "fact");
    assert_eq!(refused(b, "claims", fact("a", DeclineOverturned, d(2028, 1, 2))), "fact");
    assert_eq!(refused(b, "claims", undoing("a", ClaimDeclined, d(2028, 1, 2), d(2027, 12, 1))), "fact");
    assert_eq!(refused(b, "claims", undoing("a", AdmissionReversed, d(2028, 1, 2), d(2028, 1, 3))), "fact");
    let many = (0..=MAX_CONDUCT_FACTS).map(|n| fact(&format!("f{n}"), ClaimAccepted, d(2028, 1, 2))).collect();
    assert_eq!(b.code("claims", None, record(many)), "batch");
    assert_eq!(b.state.conduct_report(jan, None), counted);

    // Once November has closed, nothing more is counted in it.
    w.close(nov);
    assert_eq!(
        w.books.code("claims", None, record(vec![fact("late", ClaimDeclined, d(2027, 11, 28))])),
        "period_closed"
    );
    assert_eq!(w.books.state.conduct_report(nov, None).declined, 0);
}

/// Purpose: a return is worked out by the ledger from a closed period, each line showing the
/// accounts behind it, and is stored as filed. Filing needs a closed period and the right
/// authority, and the same return cannot be filed twice. When a correction is later posted
/// for that period the ledger reports that the return needs restating; the restatement is a
/// new version with a reason, shows the restating amount by the period it was posted in, and
/// leaves the first version exactly as filed. A restatement with nothing changed is refused.
/// Why it matters: a figure sent to the regulator has to be reproducible from the ledger and
/// traceable to postings, and what was sent must never be overwritten. When the past is
/// corrected, both the original and the restated figure have to exist, with the path
/// between them.
#[test]
fn a_return_is_filed_from_a_closed_period_and_restated_as_a_new_version() {
    let mut w = world(d(2027, 12, 20));
    let (nov, dec) = (period(2027, 11), period(2027, 12));
    let def = definition();
    let file = |id: &str, period| FinCommand::FileReturn {
        id: id.to_string(),
        definition: definition(),
        period,
    };
    let restate = |id: &str, reason: &str| FinCommand::RestateReturn {
        id: id.to_string(),
        definition: definition(),
        reason: reason.to_string(),
    };
    assert_eq!(w.books.console_code("olive", file("q-2027-11", nov)), "period", "November is not closed yet");
    w.books
        .send("claims", None, record(vec![fact("clm-1", ConductMeasure::ClaimDeclined, d(2027, 11, 25))]))
        .unwrap();
    let mut reserves = journal("res", JournalKind::Accrual, nov, "6000", "2400", 4_000_00);
    reserves.value_date = d(2027, 11, 30);
    w.books.console("olive", FinCommand::ProposeJournal { journal: reserves }).unwrap();
    w.books.console("ana", FinCommand::ApproveJournal { id: "res".to_string() }).unwrap();
    w.close(nov);

    let b = &mut w.books;
    assert_eq!(b.console_code("ana", file("q-2027-11", nov)), "authority");
    assert_eq!(b.code("reporting", Some("olive"), file("q-2027-11", nov)), "caller");
    assert_eq!(b.console_code("olive", file("", nov)), "return");
    assert_eq!(b.console_code("lena", restate("q-2027-11", "too early")), "return");
    b.console("olive", file("q-2027-11", nov)).unwrap();
    assert!(matches!(&b.applied.last().unwrap().events[0].event, FinEvent::ReturnFiled { version: 1, .. }));
    assert_eq!(b.console_code("olive", file("q-2027-11", nov)), "return");

    let v1 = b.state.returns["q-2027-11"][0].clone();
    let closed = b.state.periods[&nov].closed.clone().unwrap();
    assert_eq!((v1.version, v1.period, v1.filed_by.as_str(), v1.reason.clone()), (1, nov, "olive", None));
    assert_eq!(
        (v1.definition.as_str(), v1.definition_version, &v1.definition_hash),
        ("insurer-quarterly", 1, &def.content_hash)
    );
    assert_eq!(v1.trial_balance_hash, closed.trial_balance_hash);
    let line = |r: &ic_finance::returns::FiledReturn, code: &str| r.lines.iter().find(|l| l.code == code).unwrap().clone();
    assert_eq!(line(&v1, "A1").value, 3 * i128::from(MONTHLY) * FINE_PER_MINOR);
    assert_eq!(line(&v1, "L2").value, 4_000_00 * FINE_PER_MINOR, "a liability is reported as a positive figure");
    assert_eq!(
        line(&v1, "L2").as_closed.iter().collect::<Vec<_>>(),
        vec![(&"2400".to_string(), &(4_000_00 * FINE_PER_MINOR))]
    );
    assert_eq!(line(&v1, "P2").value, 4_000_00 * FINE_PER_MINOR);
    let earned = line(&v1, "P1");
    assert!(earned.value > 0 && earned.value == line(&v1, "P1L").value);
    assert_eq!(
        earned.value,
        -closed.trial_balance.iter().filter(|r| r.account == "4000").map(|r| r.balance).sum::<i128>()
    );
    assert_eq!((line(&v1, "C2").value, line(&v1, "C3").value), (1, 0));
    assert!(v1.lines.iter().all(|l| l.restated.is_empty()) && v1.restating_entries.is_empty());

    // Nothing has changed, so there is nothing to restate.
    assert!(b.state.returns_needing_restatement(std::slice::from_ref(&def)).is_empty());
    assert_eq!(b.console_code("lena", restate("q-2027-11", "no reason at all")), "unchanged");

    // In December the November reserve is found to be 500.00 short.
    let mut correction = journal("res-fix", JournalKind::Correction, dec, "6000", "2400", 500_00);
    correction.value_date = d(2027, 11, 30);
    b.console("olive", FinCommand::ProposeJournal { journal: correction }).unwrap();
    b.console("ana", FinCommand::ApproveJournal { id: "res-fix".to_string() }).unwrap();
    assert_eq!(b.state.returns_needing_restatement(std::slice::from_ref(&def)), vec!["q-2027-11".to_string()]);
    assert!(
        b.state.returns_needing_restatement(&[]).is_empty(),
        "without its definition a return cannot be recomputed"
    );

    assert_eq!(b.console_code("olive", restate("q-2027-11", "reserve corrected")), "authority");
    assert_eq!(b.console_code("lena", restate("q-2027-11", "  ")), "return");
    let mut other = definition();
    other.id = "another-return".to_string();
    let wrong = FinCommand::RestateReturn {
        id: "q-2027-11".to_string(),
        definition: other,
        reason: "reserve corrected".to_string(),
    };
    assert_eq!(b.console_code("lena", wrong), "return");
    b.console("lena", restate("q-2027-11", "November reserve corrected in December")).unwrap();
    assert!(matches!(&b.applied.last().unwrap().events[0].event, FinEvent::ReturnFiled { version: 2, .. }));

    let versions = &b.state.returns["q-2027-11"];
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0], v1, "what was filed first is exactly as it was");
    let v2 = &versions[1];
    assert_eq!(
        (v2.version, v2.filed_by.as_str(), v2.reason.as_deref()),
        (2, "lena", Some("November reserve corrected in December"))
    );
    assert_ne!(v2.hash, v1.hash);
    let restated = line(v2, "L2");
    assert_eq!(restated.as_closed, line(&v1, "L2").as_closed);
    assert_eq!(restated.restated.iter().collect::<Vec<_>>(), vec![(&dec, &(500_00 * FINE_PER_MINOR))]);
    assert_eq!(restated.value, 4_500_00 * FINE_PER_MINOR);
    assert_eq!(line(v2, "P2").value, 4_500_00 * FINE_PER_MINOR);
    assert_eq!(line(v2, "A1"), line(&v1, "A1"), "lines the correction did not touch are unchanged");
    assert!(b.state.returns_needing_restatement(&[def]).is_empty());

    // The filed returns survive a save and load with the rest of the state.
    let loaded: FinanceState = serde_json::from_slice(&serde_json::to_vec(&b.state).unwrap()).unwrap();
    assert_eq!(loaded, b.state);
}

/// Purpose: a return definition is refused when it has no ID, version or lines, a line with
/// no code or a repeated code, or a line that names no accounts; a definition naming an
/// account the chart does not have is refused when filed; and the shipped definition keeps
/// declines and overturned declines as separate lines.
/// Why it matters: a line that silently summed nothing would file a zero with the regulator.
/// The shipped template is also where the rule about not netting overturns is made concrete.
#[test]
fn a_return_definition_that_would_report_nothing_is_refused() {
    let def = definition();
    let measures: Vec<_> = def
        .lines
        .iter()
        .filter_map(|l| match &l.source {
            ic_finance::returns::LineSource::Conduct { measure } => Some((l.code.as_str(), *measure)),
            _ => None,
        })
        .collect();
    assert!(measures.contains(&("C2", ConductMeasure::ClaimDeclined)) && measures.contains(&("C3", ConductMeasure::DeclineOverturned)));
    assert_eq!(def.content_hash.len(), 64);

    let invalid = |change: &dyn Fn(&mut ReturnDefinition)| {
        let mut d = def.clone();
        change(&mut d);
        match d.validate() {
            Err(ConfigError::Invalid(message)) => message,
            other => panic!("expected a validation error, got {other:?}"),
        }
    };
    assert!(invalid(&|d| d.id.clear()).contains("needs an ID"));
    assert!(invalid(&|d| d.version = 0).contains("needs an ID"));
    assert!(invalid(&|d| d.lines.clear()).contains("at least one line"));
    assert!(invalid(&|d| d.lines[1].code = "A1".to_string()).contains("declared twice"));
    assert!(invalid(&|d| d.lines[0].code.clear()).contains("unnamed"));
    assert!(invalid(&|d| d.lines[0].source = ic_finance::returns::LineSource::Balance { accounts: vec![] }).contains("names no accounts"));
    assert!(matches!(
        ReturnDefinition::load(
            "r.star",
            "RETURN = {\"id\": \"x\", \"version\": 1, \"title\": \"x\", \"currency\": \"NZD\", \"lines\": []}"
        ),
        Err(ConfigError::Invalid(_))
    ));
    assert!(matches!(ReturnDefinition::load("r.star", "OTHER = 1"), Err(ConfigError::MissingSymbol { .. })));

    let mut w = world(d(2027, 12, 3));
    w.close(period(2027, 11));
    let file = |definition| FinCommand::FileReturn {
        id: "q".to_string(),
        definition,
        period: period(2027, 11),
    };
    let mut empty = def.clone();
    empty.lines.clear();
    assert_eq!(w.books.console_code("olive", file(empty)), "definition");
    let mut unknown = def.clone();
    unknown.lines[0].source = ic_finance::returns::LineSource::Movement {
        accounts: vec!["1000".to_string(), "0000".to_string()],
    };
    assert_eq!(w.books.console_code("olive", file(unknown)), "account");
    assert!(w.books.state.returns.is_empty());
}

/// Purpose: the regulator changes what line L2 means from December 2027. Version 1 of the
/// mapping is given December's predecessor as its last period and version 2 starts in
/// December with reserves alone on the line. November files under version 1 and is
/// refused under version 2; December files under version 2 and is refused under version
/// 1; the November return, already filed, is exactly as it was; and a mapping whose last
/// period is before its first is refused when loaded.
/// Why it matters: a definition change must not reach back into a period already reported,
/// and nobody should be able to file a period on the mapping that was not in force for
/// it. Without an effective period on the mapping both are a matter of picking the right
/// file by hand on filing day.
#[test]
fn a_changed_line_definition_is_a_new_mapping_version_from_a_period() {
    let (nov, dec) = (period(2027, 11), period(2027, 12));
    let v1 = || ReturnDefinition {
        effective_to: Some(nov),
        ..definition()
    };
    let v2 = || {
        let source = QUARTERLY
            .replace("\"version\": 1", "\"version\": 2")
            .replace("[\"2400\", \"2410\", \"2420\", \"2430\"]", "[\"2400\"]");
        ReturnDefinition {
            effective_from: Some(dec),
            ..ReturnDefinition::load("insurer_quarterly.star", &source).unwrap()
        }
    };
    assert_eq!(
        (v2().version, v2().in_effect(nov), v2().in_effect(dec), v1().in_effect(dec)),
        (2, false, true, false)
    );
    assert_ne!(v2().content_hash, v1().content_hash);
    let backwards = ReturnDefinition {
        effective_from: Some(dec),
        effective_to: Some(nov),
        ..definition()
    };
    assert!(matches!(backwards.validate(), Err(ConfigError::Invalid(_))));

    let mut w = world(d(2028, 1, 6));
    w.close(nov);
    w.close(dec);
    let b = &mut w.books;
    let file = |id: &str, definition: ReturnDefinition, period| FinCommand::FileReturn {
        id: id.to_string(),
        definition,
        period,
    };
    assert_eq!(b.console_code("olive", file("q-11", v2(), nov)), "not_effective");
    b.console("olive", file("q-11", v1(), nov)).unwrap();
    let november = b.state.returns["q-11"].clone();
    assert_eq!(b.console_code("olive", file("q-12", v1(), dec)), "not_effective");
    b.console("olive", file("q-12", v2(), dec)).unwrap();
    let december = &b.state.returns["q-12"][0];
    assert_eq!((december.definition_version, &december.definition_hash), (2, &v2().content_hash));
    assert_eq!(b.state.returns["q-11"], november, "the return already filed is untouched");
    // November cannot be restated onto the new meaning of the line either.
    let onto_v2 = FinCommand::RestateReturn {
        id: "q-11".to_string(),
        definition: v2(),
        reason: "new definition".to_string(),
    };
    assert_eq!(b.console_code("lena", onto_v2), "not_effective");
}

/// Purpose: November's return is filed, then restated after a correction. Asked for line
/// L2, the ledger answers with the mapping's ID, version and hash, the accounts the line
/// sums, the figure by account as the period closed, the amount posted since by period,
/// the hash of the closed trial balance, the question that gives the same rows today and
/// the extracts registered for the period's last day with their definition version and
/// hash. The first filing can still be asked for by its version, a conduct line points
/// at the conduct question, and a return, version or line that does not exist is named.
/// Why it matters: a figure sent to a regulator will be questioned months later by
/// someone who was not there. They need to get from the number to the query, the extract
/// and the definition behind it without asking the person who filed it.
#[test]
fn any_figure_on_a_filed_return_opens_to_what_is_behind_it() {
    let mut w = world(d(2027, 12, 20));
    let (nov, dec) = (period(2027, 11), period(2027, 12));
    let mut reserves = journal("res", JournalKind::Accrual, nov, "6000", "2400", 4_000_00);
    reserves.value_date = d(2027, 11, 30);
    w.books.console("olive", FinCommand::ProposeJournal { journal: reserves }).unwrap();
    w.books.console("ana", FinCommand::ApproveJournal { id: "res".to_string() }).unwrap();
    w.close(nov);
    let b = &mut w.books;
    let file = FinCommand::FileReturn {
        id: "q-2027-11".to_string(),
        definition: definition(),
        period: nov,
    };
    b.console("olive", file).unwrap();
    let mut correction = journal("res-fix", JournalKind::Correction, dec, "6000", "2400", 500_00);
    correction.value_date = d(2027, 11, 30);
    b.console("olive", FinCommand::ProposeJournal { journal: correction }).unwrap();
    b.console("ana", FinCommand::ApproveJournal { id: "res-fix".to_string() }).unwrap();
    let restate = FinCommand::RestateReturn {
        id: "q-2027-11".to_string(),
        definition: definition(),
        reason: "November reserve corrected in December".to_string(),
    };
    b.console("lena", restate).unwrap();

    let ask = |q: serde_json::Value| ic_finance::host::answer(&b.state, q);
    let line = ask(serde_json::json!({"q": "return_line", "id": "q-2027-11", "code": "L2"})).unwrap();
    assert_eq!(
        (line["version"].clone(), line["definition"].clone(), line["definition_version"].clone()),
        (serde_json::json!(2), serde_json::json!("insurer-quarterly"), serde_json::json!(1))
    );
    assert_eq!(line["definition_hash"], serde_json::json!(definition().content_hash));
    assert_eq!(
        line["line"]["source"],
        serde_json::json!({"source": "balance", "accounts": ["2400", "2410", "2420", "2430"]})
    );
    assert_eq!(line["line"]["as_closed"]["2400"], serde_json::json!(4_000_00 * FINE_PER_MINOR));
    assert_eq!(line["line"]["restated"]["2027-12"], serde_json::json!(500_00 * FINE_PER_MINOR));
    let closed = b.state.periods[&nov].closed.clone().unwrap();
    assert_eq!(line["trial_balance_hash"], serde_json::to_value(closed.trial_balance_hash).unwrap());
    assert_eq!(line["query"], serde_json::json!({"q": "trial_balance", "period": "2027-11", "fund": null}));
    assert_eq!(line["reason"], serde_json::json!("November reserve corrected in December"));
    let extracts = line["extracts"].as_array().unwrap();
    assert!(extracts.len() >= 2, "{extracts:?}");
    assert!(extracts
        .iter()
        .all(|x| x["as_at"] == serde_json::json!("2027-11-30") && x["definition_version"].is_u64() && x["content_hash"].is_string()));
    // The rows the query gives today add up to the line.
    let today = ask(line["query"].clone()).unwrap();
    let reserve: i128 = today
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["account"] == "2400")
        .map(|r| serde_json::from_value::<i128>(r["balance"].clone()).unwrap())
        .sum();
    assert_eq!(-reserve, 4_000_00 * FINE_PER_MINOR, "November's own trial balance holds the figure as closed");

    let first = ask(serde_json::json!({"q": "return_line", "id": "q-2027-11", "version": 1, "code": "L2"})).unwrap();
    assert_eq!(
        (first["version"].clone(), first["reason"].clone()),
        (serde_json::json!(1), serde_json::Value::Null)
    );
    assert!(first["line"]["restated"].as_object().unwrap().is_empty());
    let life = ask(serde_json::json!({"q": "return_line", "id": "q-2027-11", "code": "P1L"})).unwrap();
    assert_eq!(life["query"]["fund"], serde_json::json!("SF1-life"));
    let declined = ask(serde_json::json!({"q": "return_line", "id": "q-2027-11", "code": "C2"})).unwrap();
    assert_eq!(declined["query"], serde_json::json!({"q": "conduct", "period": "2027-11"}));
    assert_eq!(declined["line"]["source"]["measure"], serde_json::json!("claim_declined"));

    for (question, problem) in [
        (serde_json::json!({"q": "return_line", "id": "q-none", "code": "L2"}), "has not been filed"),
        (
            serde_json::json!({"q": "return_line", "id": "q-2027-11", "version": 9, "code": "L2"}),
            "no version 9",
        ),
        (serde_json::json!({"q": "return_line", "id": "q-2027-11", "code": "ZZ"}), "no line ZZ"),
    ] {
        let refused = ask(question).unwrap_err();
        assert!(refused.contains(problem), "{refused}");
    }
}

/// Purpose: a ledger runs from November 2027 with every month closed in turn until
/// January 2030 closes. A declined claim is counted in November 2027 under a command
/// key. While the month is open the same key, and the same fact under a new key, are
/// both recognised and nothing is counted twice. Once January 2030 has closed, the day
/// records and Merkle roots before January 2028 are gone and no root is asked for again;
/// November 2027's closed trial balance, its conduct count and a later month's day
/// records are all still there; and the old key and the old fact, sent again, are
/// refused because the month is closed.
/// Why it matters: the ledger's state is replicated and snapshotted on every node. The
/// records that only serve an open period (each day's control record, command keys, fact
/// IDs) would otherwise grow for the life of the insurer, and letting them go must never
/// let an old command or fact be taken a second time.
#[test]
fn working_records_are_let_go_two_years_after_a_period_closes() {
    let start = d(2027, 11, 15);
    let mut w = World::new(1, start);
    let (p, id) = w.issue(1, start);
    w.pay(p, &id, "r1", 40 * MONTHLY);
    let nov = period(2027, 11);
    let declined = || record(vec![fact("clm-1", ConductMeasure::ClaimDeclined, d(2027, 11, 25))]);
    assert_eq!(w.books.send_keyed("k-1", "claims", None, declined()).result, Ok(()));
    let again = w.books.send_keyed("k-1", "claims", None, declined());
    assert!(again.duplicate);
    assert_eq!(w.books.send_keyed("k-2", "claims", None, declined()).result, Ok(()));
    assert_eq!(w.books.state.conduct_report(nov, None).declined, 1);

    let mut month = nov;
    while month <= period(2030, 1) {
        let next = ic_finance::PeriodId::of(month.end().succ_opt().unwrap());
        w.until(next.start() + chrono::Days::new(2));
        if month < period(2030, 1) {
            assert_eq!(w.books.state.first_date(), Some(start), "trimmed before {month} closed");
        }
        w.close(month);
        month = next;
    }
    let state = &w.books.state;
    assert_eq!(state.first_date(), Some(d(2028, 1, 1)));
    assert!(state.sources(period(2027, 12)).is_empty());
    assert_eq!(state.sources(period(2028, 1)).len(), 31);
    assert!(
        state.work().roots_wanted.iter().all(|date| *date >= d(2030, 2, 1)),
        "{:?}",
        state.work().roots_wanted
    );
    let closed = state.periods[&nov].closed.clone().unwrap();
    assert_eq!(state.trial_balance(nov, None), closed.trial_balance);
    assert_eq!(state.conduct_report(nov, None).declined, 1);

    for key in ["k-1", "k-3"] {
        let late = w.books.send_keyed(key, "claims", None, declined());
        assert!(!late.duplicate);
        assert_eq!(late.result.unwrap_err().code, "period_closed");
    }
    assert_eq!(w.books.state.conduct_report(nov, None).declined, 1);
}
