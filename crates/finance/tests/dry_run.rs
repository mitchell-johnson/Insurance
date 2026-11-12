//! A proposed rate table priced against the in-force book before it is approved.

mod common;

use common::*;
use ic_core::PolicyId;
use ic_finance::host;
use ic_finance::mart::RatedCover;
use ic_finance::tables::{dry_run, DryRunRequest, DRY_RUN_LISTED};
use ic_product::{load_rate_table, RateTable, Sex};
use serde_json::{json, Value};

const LIFE: &str = include_str!("../../../products/rates/life_base.star");

/// The shipped life table, with one rate changed when asked.
fn life(from: &str, to: &str) -> RateTable {
    assert!(from.is_empty() || LIFE.contains(from), "the shipped table has no {from}");
    let mut table = load_rate_table("life_base.star", &LIFE.replacen(from, to, 1)).unwrap();
    if !from.is_empty() {
        table.version = 2;
    }
    table
}

/// The row a 37 year old non-smoking woman is rated on.
const ROW: &str = r#"rate("1.2885", age_from = 33, age_to = 37, sex = "female", smoker = False),"#;

fn cover(policy: &str, age: Option<u32>, sum_insured_minor: i64) -> RatedCover {
    RatedCover {
        partition: 0,
        policy: PolicyId(policy.to_string()),
        cover: "life".to_string(),
        layer: Some(1),
        age,
        age_at_start: age.map(|a| a - 10),
        sex: Some(Sex::Female),
        smoker: Some(false),
        occupation_class: None,
        sum_insured_minor,
    }
}

fn request(proposed: RateTable) -> DryRunRequest {
    DryRunRequest {
        covers: vec!["life".to_string()],
        per_life: false,
        age_at_start: false,
        current: life("", ""),
        proposed,
    }
}

/// Purpose: three term life policies of $500,000 on 37 year old non-smoking women are in
/// force at the end of November 2027. Asked through the module's own question, a version
/// that raises their rate from 1.2885 to 1.4174 per thousand moves each annual premium
/// from $644.25 to $708.70, and the answer gives the book total, the rating cell and the
/// three covers by policy. A version that drops their row leaves all three without a rate
/// and does not pass. A date the book has not closed is refused.
/// Why it matters: validation checks a table against its schema; this shows what it does
/// to real customers before anyone approves it. A table that passes every shape check
/// can still leave part of the book unrateable at the next anniversary, and the first
/// sign of that should not be the billing run failing.
#[test]
fn a_proposed_table_is_priced_against_the_book_in_force() {
    let start = d(2027, 11, 15);
    let mut w = World::new(2, start);
    let policies: Vec<String> = (1..=3).map(|n| w.issue(n, start).1 .0).collect();
    w.until(d(2027, 12, 2));

    let ask = |as_at: chrono::NaiveDate, proposed: RateTable| -> Result<Value, String> {
        let question = json!({ "q": "table_dry_run", "as_at": as_at, "request": request(proposed) });
        host::answer(&w.books.state, question)
    };
    let run = ask(d(2027, 11, 30), life("1.2885", "1.4174")).unwrap();
    assert_eq!(
        (run["current"].as_str(), run["proposed"].as_str()),
        (Some("life-base v1"), Some("life-base v2"))
    );
    assert_eq!(run["covers"], json!(3));
    assert_eq!(run["current_annual_minor"], json!(3 * 644_25));
    assert_eq!(run["proposed_annual_minor"], json!(3 * 708_70));
    assert_eq!(run["change_minor"], json!(3 * 64_45));
    assert_eq!(run["passed"], json!(true));
    assert_eq!(
        run["cells"],
        json!([{ "age_band": "35-39", "sex": "female", "smoker": false, "covers": 3,
                 "current_annual_minor": 3 * 644_25, "proposed_annual_minor": 3 * 708_70 }])
    );
    let moved: Vec<&str> = run["largest_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["policy"].as_str().unwrap())
        .collect();
    let mut expected: Vec<&str> = policies.iter().map(String::as_str).collect();
    let mut sorted = moved.clone();
    sorted.sort_unstable();
    expected.sort_unstable();
    assert_eq!(sorted, expected);
    assert_eq!(run["largest_changes"][0]["age"], json!(37));
    // Nothing in the answer carries a date of birth.
    assert!(!run.to_string().contains("1990"));

    // The same version against itself moves nothing.
    let same = ask(d(2027, 11, 30), life("", "")).unwrap();
    assert_eq!((same["change_minor"].clone(), same["largest_changes"].clone()), (json!(0), json!([])));

    let dropped = ask(d(2027, 11, 30), life(ROW, "")).unwrap();
    assert_eq!((dropped["covers"].clone(), dropped["left_without_a_rate"].clone()), (json!(0), json!(3)));
    assert_eq!(dropped["passed"], json!(false));
    assert_eq!(dropped["left_without_a_rate_examples"].as_array().unwrap().len(), 3);
    assert!(dropped["left_without_a_rate_examples"][0].as_str().unwrap().ends_with("/life"));

    let early = ask(d(2027, 12, 20), life("", "")).unwrap_err();
    assert!(early.contains("2027-12-20"), "{early}");
}

/// Purpose: the comparison itself, on a book written out by hand. A cover whose life has
/// no age is counted and not priced; one that neither version rates and one that only the
/// new version rates are counted apart; a rate per life ignores the sum insured; pricing
/// on the age at start uses the younger row; the covers that move are listed largest
/// first and the list stops at twenty; a row for one occupation class prices only lives
/// of that class; and half a cent rounds up.
/// Why it matters: the totals are what an actuary signs against. A cover silently left
/// out, or counted in the total under one version and not the other, makes the movement
/// wrong in a way nobody can see from the summary.
#[test]
fn the_comparison_counts_what_it_cannot_price_and_lists_the_largest_moves() {
    let as_at = d(2027, 11, 30);
    let raised = || life("1.2885", "1.4174");

    // Thirty covers of rising size, one with no age, one too old for either version.
    let mut book: Vec<RatedCover> = (1..=30).map(|n| cover(&format!("pol-{n:02}"), Some(37), n * 100_000_00)).collect();
    book.push(cover("pol-no-age", None, 100_000_00));
    book.push(cover("pol-old", Some(140), 100_000_00));
    let run = dry_run(as_at, &request(raised()), &book);
    assert_eq!((run.covers, run.no_age, run.no_rate_in_either, run.newly_rated), (30, 1, 1, 0));
    assert!(run.passed);
    assert_eq!(run.largest_changes.len(), DRY_RUN_LISTED);
    assert_eq!(run.largest_changes[0].policy, "pol-30");
    assert_eq!(run.largest_changes[19].policy, "pol-11");
    // $100,000 at 1.2885 is $128.85 a year, and the sizes add to 465 of them.
    assert_eq!(run.current_annual_minor, 465 * 128_85);
    assert_eq!(run.proposed_annual_minor, 465 * 141_74);
    assert_eq!(run.cells.len(), 1);

    // A version that starts rating a cell the current one does not.
    let mut widened = request(life("", ""));
    widened.current = life(ROW, "");
    let run = dry_run(as_at, &widened, &book[..2]);
    assert_eq!((run.covers, run.newly_rated, run.left_without_a_rate, run.passed), (0, 2, 0, true));

    // Per life: the rate is the premium, whatever the sum insured.
    let mut flat = request(raised());
    flat.per_life = true;
    let run = dry_run(as_at, &flat, &book[..2]);
    // 1.2885 is $1.29 to the cent and 1.4174 is $1.42.
    assert_eq!((run.current_annual_minor, run.proposed_annual_minor), (2 * 1_29, 2 * 1_42));

    // Level premiums are priced on the age the cover started at: 27, a row left alone.
    let mut level = request(raised());
    level.age_at_start = true;
    let run = dry_run(as_at, &level, &book[..1]);
    assert_eq!((run.covers, run.change_minor), (1, 0));
    assert_eq!(run.cells[0].age_band, "25-29");
    // $100,000 at 0.6390, the rate for a woman of 27.
    assert_eq!(run.current_annual_minor, 63_90);

    // A row for one occupation class rates only lives of that class, and comes before
    // the row for everyone else.
    let classed = || {
        let mut table = raised();
        let at = table.rows.iter().position(|r| r.annual_rate.as_str() == "1.4174").unwrap();
        let mut heavy = table.rows[at].clone();
        heavy.occupation_class = Some("4".to_string());
        heavy.annual_rate = serde_json::from_value(json!("2.5770")).unwrap();
        table.rows.insert(at, heavy);
        table
    };
    let mut manual = cover("pol-manual", Some(37), 100_000_00);
    manual.occupation_class = Some("4".to_string());
    let run = dry_run(as_at, &request(classed()), &[manual, cover("pol-office", Some(37), 100_000_00)]);
    assert_eq!(run.proposed_annual_minor, 257_70 + 141_74);
    assert_eq!(run.largest_changes[0].policy, "pol-manual");

    // A premium is rounded to the nearest cent.
    let small = [cover("a", Some(37), 1_000_00), cover("b", Some(37), 3_88_00)];
    let run = dry_run(as_at, &request(raised()), &small);
    // 388 dollars at 1.2885 per thousand is 49.9938 cents; 1,000 dollars is 128.85 cents.
    assert_eq!(run.current_annual_minor, 129 + 50);
}
