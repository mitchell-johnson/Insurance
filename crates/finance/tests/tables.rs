//! The validation suite an actuarial table must pass before activation.

use ic_finance::config::ConfigError;
use ic_finance::tables::{cross_check, load_schemas, validate, Check, Manifest, Severity, TableSchema, ValidationReport};
use ic_product::{load_rate_table, RateTable};

const SCHEMAS: &str = include_str!("../../../config/finance/tables.star");
const LIFE: &str = include_str!("../../../products/rates/life_base.star");
const TRAUMA: &str = include_str!("../../../products/rates/trauma_base.star");
const HEALTH: &str = include_str!("../../../products/rates/health_base.star");
const GROUP: &str = include_str!("../../../products/rates/group_life_standard.star");

fn schema(id: &str) -> TableSchema {
    load_schemas("tables.star", SCHEMAS).unwrap().remove(id).unwrap()
}

fn manifest() -> Manifest {
    Manifest {
        model: "prophet-term".to_string(),
        model_version: "2027.2".to_string(),
        basis_report: "vault://actuarial/basis-2027".to_string(),
        author: "aroha".to_string(),
    }
}

fn life() -> RateTable {
    load_rate_table("life_base.star", LIFE).unwrap()
}

/// The life table with one line of its source replaced.
fn life_with(from: &str, to: &str) -> RateTable {
    assert!(LIFE.contains(from), "{from} is not in the shipped table");
    load_rate_table("life_base.star", &LIFE.replacen(from, to, 1)).unwrap()
}

fn check(table: &RateTable) -> ValidationReport {
    validate(table, &schema("life-base"), &manifest(), None)
}

/// The blocking findings of one check, as (cell, message).
fn blocked(report: &ValidationReport, check: Check) -> Vec<(String, String)> {
    report
        .blocking()
        .into_iter()
        .filter(|f| f.check == check)
        .map(|f| (f.cell.clone(), f.message.clone()))
        .collect()
}

const ROW_33_FEMALE: &str = "rate(\"1.2885\", age_from = 33, age_to = 37, sex = \"female\", smoker = False)";

/// Purpose: every rate table shipped in the repository passes the validation suite against
/// the schema declared for it, with no finding of any kind, and the report records the
/// table's hash, the schema version and the manifest.
/// Why it matters: this is the gate in CI. A table edited in a pull request that breaks a
/// rule fails here, before anyone can propose activating it, and the report is the evidence
/// the activation carries.
#[test]
fn every_shipped_table_passes_its_schema() {
    let schemas = load_schemas("tables.star", SCHEMAS).unwrap();
    let shipped = [
        ("life_base.star", LIFE),
        ("trauma_base.star", TRAUMA),
        ("health_base.star", HEALTH),
        ("group_life_standard.star", GROUP),
    ];
    for (file, source) in shipped {
        let table = load_rate_table(file, source).unwrap();
        let schema = schemas.get(&table.id).unwrap_or_else(|| panic!("no schema is declared for {}", table.id));
        let report = validate(&table, schema, &manifest(), None);
        assert!(report.findings.is_empty(), "{file}: {:?}", report.findings);
        assert!(report.passed());
        assert_eq!((report.table.as_str(), report.version, report.cells), (table.id.as_str(), 1, table.rows.len()));
        assert_eq!(
            (report.content_hash.as_str(), report.schema_version, report.previous_version),
            (table.content_hash.as_str(), 1, None)
        );
        assert_eq!(report.schema_kind, schema.kind);
    }
    assert_eq!(schemas.len(), shipped.len(), "every schema has a table and every table a schema");
}

/// Purpose: a cell keyed a factor of ten too high, or too low, stays inside the table's
/// allowed range but is caught by the adjacent-age checks: the rate must not fall from one
/// age band to the next and must not jump by more than the schema's largest step.
/// Why it matters: this is the control the design addendum's third decision adds after a
/// mis-keyed cell reached customers' notices. A range check alone passes that cell. These
/// two checks are what stop it at the door.
#[test]
fn a_cell_out_by_a_factor_of_ten_fails_the_adjacent_age_checks() {
    let high = check(&life_with(ROW_33_FEMALE, &ROW_33_FEMALE.replace("1.2885", "12.885")));
    assert!(!high.passed());
    assert!(blocked(&high, Check::Range).is_empty(), "the cell is inside the allowed range");
    let smooth = blocked(&high, Check::Smoothness);
    assert_eq!(smooth.len(), 1);
    assert_eq!(smooth[0].0, "female non-smoker age 33-37");
    assert_eq!(smooth[0].1, "rate 12.885 is more than 2.5 times the 0.9074 of the band before it");
    // The band after it is now lower than it, and the smoker of the same age is cheaper.
    assert_eq!(blocked(&high, Check::Monotonicity)[0].0, "female non-smoker age 38-42");
    assert_eq!(blocked(&high, Check::SmokerLoading)[0].0, "female smoker age 33-37");

    let low = check(&life_with(ROW_33_FEMALE, &ROW_33_FEMALE.replace("1.2885", "0.1288")));
    let falls = blocked(&low, Check::Monotonicity);
    assert_eq!(
        falls,
        vec![(
            "female non-smoker age 33-37".to_string(),
            "rate 0.1288 is below 0.9074 for the band before it".to_string()
        )]
    );
    assert_eq!(
        blocked(&low, Check::Smoothness)[0].0,
        "female non-smoker age 38-42",
        "and the step back up is too steep"
    );
}

/// Purpose: a table with a missing rating cell, a gap or an overlap in its age bands, ages
/// short of or past the schema's range, a row for a cell the schema does not have, a rate
/// out of range or written to too many places, or a manifest with a blank field is blocked
/// with a finding that names the cell.
/// Why it matters: a gap in a table is a customer who cannot be rated on their birthday, and
/// an overlap is one who can be rated two ways. The actuary needs the finding to point at
/// the row, not just to say the file is wrong.
#[test]
fn shape_range_precision_and_manifest_faults_block_with_the_cell_named() {
    let gap = check(&life_with(
        "age_from = 33, age_to = 37, sex = \"female\", smoker = False",
        "age_from = 34, age_to = 37, sex = \"female\", smoker = False",
    ));
    assert_eq!(
        blocked(&gap, Check::Shape),
        vec![("female non-smoker age 34-37".to_string(), "ages 33 to 33 are not covered".to_string())]
    );

    let overlap = check(&life_with(
        "age_from = 33, age_to = 37, sex = \"female\", smoker = False",
        "age_from = 32, age_to = 37, sex = \"female\", smoker = False",
    ));
    assert_eq!(blocked(&overlap, Check::Shape)[0].1, "overlaps the band before it: age 32 is rated twice");

    let short = check(&life_with(
        "age_from = 98, age_to = 102, sex = \"male\", smoker = True",
        "age_from = 98, age_to = 100, sex = \"male\", smoker = True",
    ));
    assert_eq!(
        blocked(&short, Check::Shape),
        vec![("male smoker".to_string(), "ages 101 to 102 are not covered".to_string())]
    );
    let long = check(&life_with(
        "age_from = 98, age_to = 102, sex = \"male\", smoker = True",
        "age_from = 98, age_to = 110, sex = \"male\", smoker = True",
    ));
    assert_eq!(blocked(&long, Check::Shape)[0].1, "rates run past the schema's last age 102");

    // Every male smoker row dropped: the cell is missing altogether.
    let mut none = life();
    none.rows.retain(|r| !(r.sex == Some(ic_product::Sex::Male) && r.smoker == Some(true)));
    assert_eq!(
        blocked(&check(&none), Check::Shape),
        vec![("male smoker".to_string(), "no rows for this rating cell".to_string())]
    );

    let stray = check(&life_with(
        ROW_33_FEMALE,
        "rate(\"1.2885\", age_from = 33, age_to = 37, sex = \"female\", smoker = False, category = \"staff\")",
    ));
    assert_eq!(
        blocked(&stray, Check::Schema)[0],
        (
            "female non-smoker staff age 33-37".to_string(),
            "the schema has no such rating cell".to_string()
        )
    );
    assert_eq!(blocked(&stray, Check::Shape)[0].1, "ages 33 to 37 are not covered");

    let range = check(&life_with(ROW_33_FEMALE, &ROW_33_FEMALE.replace("1.2885", "0.00001")));
    assert_eq!(blocked(&range, Check::Range)[0].1, "rate 0.00001 is outside 0.0001 to 1000");
    assert_eq!(blocked(&range, Check::Precision)[0].1, "rate 0.00001 has 5 decimal places; the schema allows 4");
    let exponent = check(&life_with(ROW_33_FEMALE, &ROW_33_FEMALE.replace("1.2885", "1.2885e0")));
    assert_eq!(
        blocked(&exponent, Check::Precision)[0].1,
        "rate `1.2885e0` is not a plain decimal of at most 9 places"
    );

    let mut unsigned = manifest();
    unsigned.author = " ".to_string();
    unsigned.basis_report.clear();
    let report = validate(&life(), &schema("life-base"), &unsigned, None);
    let missing: Vec<String> = blocked(&report, Check::Manifest).into_iter().map(|(_, m)| m).collect();
    assert_eq!(
        missing,
        vec![
            "the manifest does not name the basis_report".to_string(),
            "the manifest does not name the author".to_string()
        ]
    );
}

/// Purpose: a table rated by category and not by age must have exactly one row for each
/// declared category and none carrying an age band.
/// Why it matters: group scheme rates are keyed by category. A duplicated category would
/// make the rate depend on row order, and a missing one would leave members unbillable.
#[test]
fn a_category_table_has_one_row_for_each_category() {
    let group = |source: &str| validate(&load_rate_table("g.star", source).unwrap(), &schema("group-life-standard"), &manifest(), None);
    let twice = group(&GROUP.replace("category = \"executive\"", "category = \"staff\""));
    let shape = blocked(&twice, Check::Shape);
    assert_eq!(
        shape,
        vec![
            ("executive".to_string(), "no rows for this rating cell".to_string()),
            ("staff".to_string(), "2 rows for one rating cell".to_string())
        ]
    );
    let aged = group(&GROUP.replace(
        "rate(\"1.10\", category = \"staff\")",
        "rate(\"1.10\", category = \"staff\", age_from = 18, age_to = 64)",
    ));
    assert_eq!(
        blocked(&aged, Check::Schema)[0].1,
        "the table is not rated by age but the row carries an age band"
    );
    let unkeyed = group(&GROUP.replace("rate(\"1.10\", category = \"staff\")", "rate(\"1.10\")"));
    assert_eq!(blocked(&unkeyed, Check::Schema)[0].0, "all");
}

/// Purpose: against the version it replaces, a new table must be the next version of the
/// same table and not take effect earlier. A uniform 5% increase passes with nothing listed,
/// a 20% move in one cell is listed for the actuary to confirm without blocking, a cell that
/// moves by more than the blocking factor either way blocks, and cells added or dropped are
/// listed.
/// Why it matters: a change that moves every cell is probably intended and one that moves a
/// single cell by a factor of ten is probably a typo. Telling the two apart before the dry
/// run is what lets the actuary sign off on the effect instead of proof-reading rows.
#[test]
fn a_new_version_is_compared_cell_by_cell_with_the_one_it_replaces() {
    let v1 = life();
    let next = |change: &dyn Fn(&mut RateTable)| {
        let mut t = v1.clone();
        t.version = 2;
        change(&mut t);
        validate(&t, &schema("life-base"), &manifest(), Some(&v1))
    };
    let set = |t: &mut RateTable, row: usize, rate: &str| t.rows[row].annual_rate = ic_money::DecStr::parse(rate).unwrap();
    let unchanged = next(&|_| {});
    assert!(unchanged.findings.is_empty());
    assert_eq!(unchanged.previous_version, Some(1));

    // Row 12 is female non-smoker 33-37 at 1.2885.
    let nudged = next(&|t| set(t, 12, "1.3400"));
    assert!(nudged.findings.is_empty(), "4% is inside the review tolerance");
    let moved = next(&|t| set(t, 12, "1.5500"));
    assert!(moved.passed());
    assert_eq!(moved.findings.len(), 1);
    assert_eq!((moved.findings[0].check, moved.findings[0].severity), (Check::Change, Severity::Review));
    assert_eq!(
        (moved.findings[0].cell.as_str(), moved.findings[0].message.as_str()),
        ("female non-smoker age 33-37", "up 20% from the previous version")
    );
    let fell = next(&|t| set(t, 12, "1.0000"));
    assert_eq!(fell.findings[0].message, "down 22% from the previous version");

    // Row 67 is the last and highest: male smoker 98-102. Only the comparison can catch it.
    let typo = next(&|t| set(t, 67, "999.0000"));
    assert_eq!(
        blocked(&typo, Check::Change),
        vec![(
            "male smoker age 98-102".to_string(),
            "moved by more than a factor of 3 from the previous version".to_string()
        )]
    );
    assert!(blocked(&typo, Check::Smoothness).len() == 1 && !typo.passed());
    let collapsed = next(&|t| set(t, 0, "0.1000"));
    assert_eq!(blocked(&collapsed, Check::Change)[0].0, "female non-smoker age 18-22");

    let rebanded = next(&|t| {
        t.rows[64].age_to = 100;
        let mut extra = t.rows[64].clone();
        (extra.age_from, extra.age_to) = (101, 102);
        t.rows.push(extra);
    });
    let notes: Vec<&str> = rebanded.findings.iter().map(|f| f.message.as_str()).collect();
    assert!(rebanded.passed());
    assert_eq!(notes.iter().filter(|m| **m == "new cell: the previous version has no rate for it").count(), 2);
    assert!(notes.contains(&"1 cells of the previous version have no rate in this one"));

    let skipped = next(&|t| t.version = 3);
    assert_eq!(blocked(&skipped, Check::Version)[0].1, "version 3 cannot follow version 1");
    let early = next(&|t| t.effective_from = t.effective_from.pred_opt().unwrap());
    assert_eq!(blocked(&early, Check::Version)[0].1, "takes effect before the version it replaces");
    let other = validate(&load_rate_table("t.star", TRAUMA).unwrap(), &schema("trauma-base"), &manifest(), Some(&v1));
    assert_eq!(blocked(&other, Check::Version)[0].1, "compared with life-base, which is a different table");
}

/// Purpose: two tables that must line up, such as a premium table and the reinsurer's rate
/// table for the same product, are checked to cover the same rating cells over the same
/// ages, and each mismatch names the cell and which table is short.
/// Why it matters: a policy the premium table can rate and the reinsurer's table cannot is
/// a risk written with no cession. That surfaces months later as a treaty dispute unless
/// the tables are checked against each other before either is activated.
#[test]
fn tables_that_must_line_up_are_checked_against_each_other() {
    let (premium, trauma) = (life(), load_rate_table("t.star", TRAUMA).unwrap());
    assert!(cross_check(&premium, &trauma).is_empty());

    let mut reinsurer = premium.clone();
    reinsurer.id = "life-reinsurer".to_string();
    reinsurer.rows.retain(|r| r.age_from < 98);
    let short = cross_check(&premium, &reinsurer);
    assert_eq!(short.len(), 4);
    assert!(short.iter().all(|f| f.check == Check::CrossTable && f.severity == Severity::Blocking));
    assert_eq!(short[0].message, "life-base covers ages 18-102 and life-reinsurer covers 18-97");

    reinsurer.rows.retain(|r| r.smoker == Some(false));
    let missing = cross_check(&premium, &reinsurer);
    assert!(missing
        .iter()
        .any(|f| f.cell == "female smoker" && f.message == "life-reinsurer has no rates for this cell"));
    let reverse = cross_check(&reinsurer, &premium);
    assert!(reverse
        .iter()
        .any(|f| f.cell == "male smoker" && f.message == "life-reinsurer has no rates for this cell"));
}

/// Purpose: a schema file is refused when a schema has no version, bounds or a step that
/// are not plain decimals, ages out of order, or a blocking change factor below two; a
/// schema that reaches validation with unusable bounds blocks every table rather than
/// passing it unchecked.
/// Why it matters: the rules are configuration too. A schema typo that silently disabled
/// the range or smoothness check would let through exactly the tables the suite exists to
/// stop.
#[test]
fn a_schema_that_cannot_check_anything_is_refused() {
    let bad = |from: &str, to: &str| {
        assert!(SCHEMAS.contains(from));
        load_schemas("tables.star", &SCHEMAS.replacen(from, to, 1))
    };
    for (from, to) in [
        ("\"version\": 1,", "\"version\": 0,"),
        ("\"rate_min\": \"0.0001\"", "\"rate_min\": \"1e-4\""),
        ("\"rate_max\": rate_max", "\"rate_max\": \"-5\""),
        ("\"max_age_step\": \"2.5\"", "\"max_age_step\": \"2.5e0\""),
        ("\"ages\": [18, 102]", "\"ages\": [102, 18]"),
        ("\"change_block_factor\": 3", "\"change_block_factor\": 1"),
    ] {
        assert!(matches!(bad(from, to), Err(ConfigError::Invalid(_))), "{to}");
    }
    assert!(matches!(bad("\"by_sex\": True,", "\"by_gender\": True,"), Err(ConfigError::Schema { .. })));

    let mut broken = schema("life-base");
    broken.rate_max = ic_money::DecStr::parse("1e3").unwrap();
    broken.max_age_step = Some(ic_money::DecStr::parse("0.1234567891").unwrap());
    let report = validate(&life(), &broken, &manifest(), None);
    let faults: Vec<String> = blocked(&report, Check::Schema).into_iter().map(|(_, m)| m).collect();
    assert_eq!(
        faults,
        vec![
            "the schema's rate bounds are not plain decimals".to_string(),
            "the schema's largest age step is not a plain decimal".to_string()
        ]
    );
    assert!(!report.passed());
}
