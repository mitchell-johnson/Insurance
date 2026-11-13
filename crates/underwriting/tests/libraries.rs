//! The configuration libraries: code lists, question sets, rules, authority
//! profiles and the Cedar policy that enforces them.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, Utc};
use ic_underwriting::authority::{load_profiles, Authority, AuthorityError, AuthorityProfile, CaseAction, Standing, Subject};
use ic_underwriting::case::{Case, CaseType, ChaserFrom, Escalation, Queue, Timers};
use ic_underwriting::codes::{load_exclusions, load_reasons, reason, well_formed_tag, AppliedExclusion, MatchOutcome};
use ic_underwriting::evidence::EvidenceKind;
use ic_underwriting::load::LoadError;
use ic_underwriting::questions::{load_question_set, Answer, AnswerProblem, DisclosureRegime};
use ic_underwriting::rules::{backtest, load_rule_set, BacktestCase, Disposition, RuleInput};
use ic_underwriting::shipped;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn t(hours: i64) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-11T00:00:00Z").unwrap().with_timezone(&Utc) + Duration::hours(hours)
}

fn invalid(r: Result<impl std::fmt::Debug, LoadError>, needle: &str) {
    match r {
        Err(LoadError::Invalid { error, .. }) => assert!(error.contains(needle), "`{error}` does not mention `{needle}`"),
        other => panic!("expected an invalid file mentioning `{needle}`, got {other:?}"),
    }
}

fn tags(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Purpose: everything shipped under `products/underwriting` loads, the rule set agrees with
/// both question sets and both code lists, and the two question sets sit either side of the
/// date the Contracts of Insurance Act 2024 applies.
/// Why it matters: a deployment starts from these files; a rule naming a question or code that
/// does not exist would fail at the first application instead of at build.
#[test]
fn shipped_configuration_loads_and_is_consistent() {
    let s = shipped::load().unwrap();
    assert!(s.exclusions.contains("EX-1001") && s.exclusions.contains("EX-9000"));
    assert!(reason::REQUIRED.iter().all(|c| s.reasons.contains(c)));
    assert_eq!(s.question_sets[0].regime, DisclosureRegime::DutyOfDisclosure);
    assert_eq!(s.question_sets[1].regime, DisclosureRegime::ReasonableCare);
    assert!(s.question_sets[0].applies_on(d(2027, 11, 14)) && !s.question_sets[0].applies_on(d(2027, 11, 15)));
    assert!(s.question_sets[1].applies_on(d(2027, 11, 15)) && !s.question_sets[1].applies_on(d(2027, 11, 14)));
    // Under reasonable care only what is asked counts: no catch-all question.
    assert!(s.question_sets[0].question("anything_else").is_some() && s.question_sets[1].question("anything_else").is_none());
    assert!(s.rule_sets[0].applies_on(d(2026, 11, 11)) && !s.rule_sets[0].applies_on(d(2025, 12, 31)));
    assert_eq!(s.grades.len(), 4);
    assert_eq!(s.wordings.clause("EX-1001").unwrap().0, "Spine");
    assert!(s.wordings.clause("EX-0000").is_none());
    assert_eq!(
        format!("{:?}", s.wordings),
        format!("ExclusionWordings({} clauses)", s.exclusions.entries.len())
    );
    assert_eq!(s.exclusions.content_hash.len(), 64);
}

fn exclusions_with(entry: &str) -> String {
    format!("EXCLUSIONS = exclusion_library('x', 1, '2026-01-01', [{entry}])")
}

/// Purpose: an exclusion list with a malformed code, a verbatim clause outside EX-9xxx, missing
/// wording, an unknown cover type, no cause tags, a cause tag outside the claims taxonomy, a
/// repeated code, or no codes at all is refused.
/// Why it matters: an exclusion code is on a policy for decades and is matched against claim
/// causes; a code the claims module cannot match would silently never apply.
#[test]
fn exclusion_list_is_validated_at_load() {
    let ok = "exclusion('EX-1001', 1, 'body_system', ['health'], ['musculoskeletal.spine'], 'Spine', 'words')";
    assert!(load_exclusions("f", &exclusions_with(ok)).is_ok());
    invalid(
        load_exclusions("f", "EXCLUSIONS = exclusion_library('x', 1, '2026-01-01', [])"),
        "at least one code",
    );
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("EX-1001", "EX-101"))), "four digits");
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("EX-1001", "EX-9001"))), "verbatim");
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("'body_system'", "'verbatim'"))), "verbatim");
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("'words'", "''"))), "wording");
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("['health']", "['pets']"))), "cover types");
    invalid(load_exclusions("f", &exclusions_with(&ok.replace("['health']", "[]"))), "cover types");
    invalid(
        load_exclusions("f", &exclusions_with(&ok.replace("['musculoskeletal.spine']", "[]"))),
        "cause tags",
    );
    invalid(
        load_exclusions("f", &exclusions_with(&ok.replace("musculoskeletal.spine", "teeth"))),
        "taxonomy",
    );
    invalid(
        load_exclusions("f", &exclusions_with(&ok.replace("musculoskeletal.spine", "unknown"))),
        "taxonomy",
    );
    invalid(
        load_exclusions("f", &exclusions_with(&ok.replace("musculoskeletal.spine", "Injury..x"))),
        "taxonomy",
    );
    invalid(load_exclusions("f", &exclusions_with(&format!("{ok}, {ok}"))), "twice");
    assert!(well_formed_tag("travel.nz") && !well_formed_tag("") && !well_formed_tag("a..b") && !well_formed_tag("A"));
}

/// Purpose: matching an applied exclusion against a claim's cause tags: a tag matches itself and
/// its descendants; dates and cover type limit it; an unknown code, a verbatim clause, no tags
/// or an unrecognised tag all go to an assessor.
/// Why it matters: this is the function the claims module calls at the cover check. Paying a
/// claim an exclusion catches, or refusing one it does not, are both failures; anything
/// uncertain must reach a person, never default.
#[test]
fn exclusion_matching_errs_towards_an_assessor() {
    let lib = shipped::load().unwrap().exclusions;
    let on = |code: &str| AppliedExclusion {
        code: code.to_string(),
        from: d(2026, 1, 1),
        until: Some(d(2028, 1, 1)),
    };
    let event = d(2027, 6, 1);
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["musculoskeletal.spine"]), event),
        MatchOutcome::Applies
    );
    assert_eq!(
        lib.matches(&on("EX-1009"), "health", &tags(&["musculoskeletal.knee"]), event),
        MatchOutcome::Applies
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["musculoskeletal.spinex"]), event),
        MatchOutcome::DoesNotApply
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["musculoskeletal"]), event),
        MatchOutcome::DoesNotApply
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["cancer", "musculoskeletal.spine"]), event),
        MatchOutcome::Applies
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "life", &tags(&["musculoskeletal.spine"]), event),
        MatchOutcome::DoesNotApply
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["musculoskeletal.spine"]), d(2025, 12, 31)),
        MatchOutcome::DoesNotApply
    );
    assert_eq!(
        lib.matches(&on("EX-1001"), "health", &tags(&["musculoskeletal.spine"]), d(2028, 1, 1)),
        MatchOutcome::DoesNotApply
    );
    assert_eq!(lib.matches(&on("EX-1001"), "health", &[], event), MatchOutcome::NeedsAssessor);
    assert_eq!(lib.matches(&on("EX-1001"), "health", &tags(&["unknown"]), event), MatchOutcome::NeedsAssessor);
    assert_eq!(lib.matches(&on("EX-1001"), "health", &tags(&["teeth"]), event), MatchOutcome::NeedsAssessor);
    assert_eq!(lib.matches(&on("EX-9000"), "health", &tags(&["cancer"]), event), MatchOutcome::NeedsAssessor);
    assert_eq!(lib.matches(&on("EX-7777"), "health", &tags(&["cancer"]), event), MatchOutcome::NeedsAssessor);
}

/// Purpose: a reason list with a malformed code, a disclosable reason without words, a withheld
/// reason with words, a repeated code, no id, or without the codes the module itself gives is
/// refused; reasons split into what the customer is told and what is withheld.
/// Why it matters: reasons are the explanation a declined applicant is entitled to; a missing
/// module code would leave a lapse or referral unexplained, and a withheld reason must never
/// reach a notice.
#[test]
fn reason_list_is_validated_and_splits_for_the_customer() {
    let lib = shipped::load().unwrap().reasons;
    let codes = vec!["RC-0101".to_string(), "RC-0190".to_string(), "RC-7777".to_string()];
    let (told, withheld) = lib.for_customer(&codes);
    assert_eq!(told, vec!["Your medical history.".to_string()]);
    assert_eq!(withheld, vec!["RC-0190".to_string(), "RC-7777".to_string()]);
    assert_eq!(lib.first_unknown(&codes), Some(&codes[2]));

    let with = |entry: &str| format!("REASONS = reason_library('r', 1, '2026-01-01', [{entry}])");
    invalid(load_reasons("f", &with("reason('RC-1', 'routing', True, 'x')")), "four digits");
    invalid(load_reasons("f", &with("reason('RC-0001', 'routing', True, '')")), "plain words");
    invalid(load_reasons("f", &with("reason('RC-0001', 'routing', False, 'x')")), "plain words");
    invalid(
        load_reasons("f", &with("reason('RC-0001', 'routing', True, 'x'), reason('RC-0001', 'routing', True, 'y')")),
        "twice",
    );
    invalid(load_reasons("f", &with("reason('RC-0001', 'routing', True, 'x')")), "must define");
    invalid(load_reasons("f", "REASONS = reason_library('', 1, '2026-01-01', [])"), "an id");
}

fn questions_with(body: &str) -> String {
    format!("QUESTIONS = question_set('q', 1, '2026-01-01', 'duty_of_disclosure', [{body}])")
}

/// Purpose: a question set with no questions, an end before its start, a question without an
/// id, a repeated question, an inverted range or a choice with no options is refused; so is a
/// risk code on a clinical question, on an answer the question cannot give, or outside the
/// pursuit, occupation and travel families.
/// Why it matters: the question set is the legal record of what was asked; a set that cannot be
/// answered consistently cannot support a non-disclosure finding later. Risk codes go to the
/// reinsurer: one tied to a health answer would send clinical information out of the module.
#[test]
fn question_set_is_validated_at_load() {
    assert!(load_question_set("f", &questions_with("yes_no('a')")).is_ok());
    invalid(load_question_set("f", &questions_with("")), "at least one question");
    invalid(
        load_question_set(
            "f",
            "QUESTIONS = question_set('q', 1, '2026-01-01', 'duty_of_disclosure', [yes_no('a')], effective_to = '2026-01-01')",
        ),
        "ends before",
    );
    invalid(load_question_set("f", &questions_with("yes_no('')")), "an id");
    invalid(load_question_set("f", &questions_with("yes_no('a'), yes_no('a')")), "twice");
    invalid(load_question_set("f", &questions_with("number('n', 5, 1)")), "inverted");
    invalid(load_question_set("f", &questions_with("choice('c', [])")), "no options");

    let tagged = load_question_set(
        "f",
        &questions_with("yes_no('p', clinical = False, risk_tag = 'pursuit.any'), choice('k', ['a', 'b'], clinical = False, risk_tags = {'a': 'travel.far'})"),
    )
    .unwrap();
    let answers = |p: bool, k: &str| BTreeMap::from([("p".to_string(), (1, Answer::YesNo(p))), ("k".to_string(), (1, Answer::Choice(k.into())))]);
    assert_eq!(
        tagged.risk_tags(&answers(true, "a")),
        BTreeSet::from(["pursuit.any".to_string(), "travel.far".to_string()])
    );
    assert!(tagged.risk_tags(&answers(false, "b")).is_empty());
    invalid(load_question_set("f", &questions_with("yes_no('h', risk_tag = 'pursuit.any')")), "never sent");
    invalid(
        load_question_set("f", &questions_with("choice('k', ['a'], clinical = False, risk_tags = {'z': 'travel.far'})")),
        "no answer z",
    );
    invalid(
        load_question_set(
            "f",
            &questions_with("number('n', 1, 5, clinical = False) | {'risk_tags': {'yes': 'travel.far'}}"),
        ),
        "no answer yes",
    );
    for bad in ["cardiovascular.heart", "pursuit", "pursuit.Diving", "pursuit..x"] {
        invalid(
            load_question_set("f", &questions_with(&format!("yes_no('p', clinical = False, risk_tag = '{bad}')"))),
            "not a pursuit, occupation or travel tag",
        );
    }
}

/// Purpose: answers are checked against the question set: unknown question, stale question
/// version, wrong kind, out of range number, negative money, an option not offered, and a
/// required question left out of a complete statement. A supplement need not be complete.
/// Why it matters: the rules read structured answers; an answer of the wrong shape would make a
/// rule silently not fire, which is an unintended acceptance at standard rates.
#[test]
fn answers_are_checked_against_the_question_set() {
    let set = load_question_set(
        "f",
        &questions_with("yes_no('y'), number('n', 1, 9), choice('c', ['a', 'b']), text('t'), money('m')"),
    )
    .unwrap();
    let mut answers: BTreeMap<String, (u32, Answer)> = BTreeMap::new();
    answers.insert("y".into(), (1, Answer::YesNo(true)));
    answers.insert("n".into(), (1, Answer::Number(5)));
    answers.insert("c".into(), (1, Answer::Choice("a".into())));
    answers.insert("t".into(), (1, Answer::Text("words".into())));
    answers.insert("m".into(), (1, Answer::Money(100)));
    assert!(set.check(&answers, true).is_empty());

    answers.insert("n".into(), (1, Answer::Number(10)));
    answers.insert("c".into(), (1, Answer::Choice("z".into())));
    answers.insert("m".into(), (1, Answer::Money(-1)));
    answers.insert("y".into(), (2, Answer::YesNo(true)));
    answers.insert("t".into(), (1, Answer::YesNo(true)));
    answers.insert("zz".into(), (1, Answer::YesNo(true)));
    let problems = set.check(&answers, true);
    let out_of_range = |q: &str| AnswerProblem::OutOfRange { question: q.into() };
    assert!(problems.contains(&out_of_range("n")) && problems.contains(&out_of_range("c")) && problems.contains(&out_of_range("m")));
    assert!(problems.contains(&AnswerProblem::WrongVersion {
        question: "y".into(),
        expected: 1
    }));
    assert!(problems.contains(&AnswerProblem::WrongKind { question: "t".into() }));
    assert!(problems.contains(&AnswerProblem::UnknownQuestion { question: "zz".into() }));

    let only: BTreeMap<String, (u32, Answer)> = [("y".to_string(), (1, Answer::YesNo(false)))].into();
    assert!(set.check(&only, false).is_empty());
    let missing = set.check(&only, true);
    assert_eq!(missing.len(), 3, "n, c and m are required; t is not");
    assert!(missing.contains(&AnswerProblem::Missing { question: "m".into() }));
}

fn rules_with(body: &str) -> String {
    format!("RULES = rule_set('r', 1, '2026-01-01', 'personal-statement', ['term-life'], [{body}])")
}

/// Purpose: a rule set without products, ending before it starts, with a repeated rule, a rule
/// whose id is not an opaque code or that does not say what it is about, a rule
/// with no reason, a zero or excessive loading, or a deferral of no months is refused; and a
/// rule set that names another question set, an unknown question, reason or exclusion fails its
/// reference check.
/// Why it matters: every outcome must explain itself with a reason code and every exclusion
/// must exist in the code list, or a decision could not be explained or applied at claim.
#[test]
fn rule_set_is_validated_at_load_and_against_its_references() {
    let s = shipped::load().unwrap();
    let good = "rule('UR-0001', answered_yes('diabetes'), loading(25), ['RC-0101'], about = 'a test')";
    let set = load_rule_set("f", &rules_with(good)).unwrap();
    assert_eq!(set.check_references(&s.question_sets[0], &s.exclusions, &s.reasons), Ok(()));
    invalid(load_rule_set("f", "RULES = rule_set('r', 1, '2026-01-01', 'q', [], [])"), "products");
    invalid(
        load_rule_set("f", "RULES = rule_set('r', 1, '2026-01-01', 'q', ['p'], [], effective_to = '2025-01-01')"),
        "ends before",
    );
    invalid(load_rule_set("f", &rules_with(&format!("{good}, {good}"))), "twice");
    // An id that names a condition would put it in the log; one is needed, and so is the why.
    invalid(
        load_rule_set("f", &rules_with("rule('diabetes', always(), loading(25), ['RC-0101'], about = 'x')")),
        "opaque",
    );
    invalid(
        load_rule_set("f", &rules_with("rule('UR-01', always(), loading(25), ['RC-0101'], about = 'x')")),
        "opaque",
    );
    invalid(load_rule_set("f", &rules_with("rule('UR-0001', always(), loading(25), ['RC-0101'])")), "about");
    invalid(
        load_rule_set("f", &rules_with("rule('UR-0001', always(), decline(), [], about = 'a test')")),
        "no reason",
    );
    invalid(
        load_rule_set("f", &rules_with("rule('UR-0001', always(), loading(0), ['RC-0101'], about = 'a test')")),
        "loading",
    );
    invalid(
        load_rule_set("f", &rules_with("rule('UR-0001', always(), loading(501), ['RC-0101'], about = 'a test')")),
        "loading",
    );
    invalid(
        load_rule_set("f", &rules_with("rule('UR-0001', always(), defer(0), ['RC-0101'], about = 'a test')")),
        "period",
    );

    let refs = |body: &str| {
        load_rule_set("f", &rules_with(body))
            .unwrap()
            .check_references(&s.question_sets[0], &s.exclusions, &s.reasons)
            .unwrap_err()
    };
    assert!(refs("rule('UR-0001', not_(all_of(answered_yes('nope'))), decline(), ['RC-0101'], about = 'a test')").contains("question nope"));
    assert!(refs("rule('UR-0001', any_of(at_least('nope', 1)), decline(), ['RC-0101'], about = 'a test')").contains("question nope"));
    assert!(refs("rule('UR-0001', at_most('nope', 1), decline(), ['RC-0101'], about = 'a test')").contains("question nope"));
    assert!(refs("rule('UR-0001', unanswered('nope'), decline(), ['RC-0101'], about = 'a test')").contains("question nope"));
    assert!(refs("rule('UR-0001', always(), decline(), ['RC-7777'], about = 'a test')").contains("reason RC-7777"));
    assert!(refs("rule('UR-0001', always(), exclude('EX-7777'), ['RC-0101'], about = 'a test')").contains("exclusion EX-7777"));
    let other = load_rule_set("f", "RULES = rule_set('r', 1, '2026-01-01', 'other', ['p'], [])").unwrap();
    assert!(other
        .check_references(&s.question_sets[0], &s.exclusions, &s.reasons)
        .unwrap_err()
        .contains("reads question set"));
}

fn input<'a>(answers: &'a BTreeMap<String, Answer>, age: u32, cover_type: &'a str, sum: i64, existing: i64) -> RuleInput<'a> {
    RuleInput {
        answers,
        age,
        smoker: Some(false),
        cover_type,
        sum_insured_minor: sum,
        existing_on_life_minor: existing,
    }
}

/// Purpose: the shipped rules over representative answers: clean answers pass straight through;
/// a smoker is loaded; a back condition is excluded on trauma but not on life; diabetes asks
/// for a blood test; mental health asks for an interview; a heart condition refers; awaiting
/// tests defers; age 75 declines; and cover already held counts towards the financial limit.
/// Why it matters: these dispositions decide who is accepted with no person involved. The order
/// of seriousness (decline over defer over refer over interview over evidence over terms) is
/// what stops a serious answer being accepted because a milder rule also fired.
#[test]
fn shipped_rules_give_the_expected_dispositions() {
    let rules = &shipped::load().unwrap().rule_sets[0];
    let base: BTreeMap<String, Answer> = [
        ("smoked_last_12_months", Answer::YesNo(false)),
        ("alcohol_units_per_week", Answer::Number(5)),
        ("weight_kg", Answer::Number(80)),
    ]
    .map(|(k, v)| (k.to_string(), v))
    .into();
    let with = |extra: &[(&str, Answer)]| {
        let mut a = base.clone();
        a.extend(extra.iter().map(|(k, v)| (k.to_string(), v.clone())));
        a
    };
    let yes = Answer::YesNo(true);
    assert_eq!(rules.assess(&input(&base, 40, "life", 500_000_00, 0)).disposition(), Disposition::Clean);

    let a = rules.assess(&input(
        &with(&[("smoked_last_12_months", yes.clone()), ("alcohol_units_per_week", Answer::Number(25))]),
        40,
        "life",
        500_000_00,
        0,
    ));
    assert_eq!((a.disposition(), a.loading_percent), (Disposition::Terms, 75), "loadings add");
    assert!(a.reasons.contains("RC-0103") && a.fired == vec!["UR-0001".to_string(), "UR-0003".to_string()]);

    let back = with(&[("back_or_neck_last_5_years", yes.clone())]);
    assert_eq!(
        rules.assess(&input(&back, 40, "trauma", 100_000_00, 0)).exclusions,
        BTreeSet::from(["EX-1001".to_string()])
    );
    assert_eq!(rules.assess(&input(&back, 40, "life", 100_000_00, 0)).disposition(), Disposition::Clean);

    let a = rules.assess(&input(&with(&[("diabetes", yes.clone())]), 40, "life", 100_000_00, 0));
    assert_eq!(
        (a.disposition(), a.evidence.clone()),
        (Disposition::NeedsEvidence, BTreeSet::from([EvidenceKind::BloodTest]))
    );
    assert_eq!(
        rules
            .assess(&input(
                &with(&[("mental_health_last_5_years", yes.clone()), ("diabetes", yes.clone())]),
                40,
                "life",
                1,
                0
            ))
            .disposition(),
        Disposition::TeleInterview
    );
    let a = rules.assess(&input(
        &with(&[("heart_condition", yes.clone()), ("mental_health_last_5_years", yes.clone())]),
        40,
        "life",
        1,
        0,
    ));
    assert_eq!((a.disposition(), a.queues.clone()), (Disposition::Refer, BTreeSet::from([Queue::Medical])));
    let a = rules.assess(&input(
        &with(&[("awaiting_tests_or_results", yes.clone()), ("heart_condition", yes.clone())]),
        40,
        "life",
        1,
        0,
    ));
    assert_eq!((a.disposition(), a.defer_months), (Disposition::Defer, Some(6)));
    assert_eq!(
        rules
            .assess(&input(&with(&[("awaiting_tests_or_results", yes.clone())]), 75, "life", 1, 0))
            .disposition(),
        Disposition::Decline
    );

    // Cover already held with the insurer counts towards the financial evidence limit.
    assert_eq!(rules.assess(&input(&base, 40, "life", 1_000_000_00, 0)).disposition(), Disposition::Clean);
    assert_eq!(
        rules.assess(&input(&base, 40, "life", 1_000_000_00, 600_000_00)).disposition(),
        Disposition::NeedsEvidence
    );
    assert_eq!(
        rules.assess(&input(&base, 40, "life", 1_000_000_00, 2_500_000_00)).disposition(),
        Disposition::Refer
    );

    let pursuit = with(&[("hazardous_pursuits", yes.clone()), ("pursuit_kind", Answer::Choice("diving".into()))]);
    assert!(rules.assess(&input(&pursuit, 40, "life", 1, 0)).exclusions.contains("EX-4003"));
    assert_eq!(
        rules.assess(&input(&with(&[("hazardous_pursuits", yes)]), 40, "life", 1, 0)).queues,
        BTreeSet::from([Queue::Occupational])
    );
    let mut smoker = input(&base, 62, "life", 600_000_00, 0);
    smoker.smoker = Some(true);
    let a = rules.assess(&smoker);
    assert_eq!((a.loading_percent, a.evidence.contains(&EvidenceKind::MedicalExam)), (50, true));
    assert_eq!(
        [
            Disposition::Clean,
            Disposition::Terms,
            Disposition::NeedsEvidence,
            Disposition::TeleInterview,
            Disposition::Refer,
            Disposition::Defer,
            Disposition::Decline
        ]
        .map(Disposition::key),
        ["clean", "terms", "needs_evidence", "tele_interview", "refer", "defer", "decline"]
    );
}

/// Purpose: a backtest runs the current and a proposed rule set over the same anonymised
/// history and reports the outcome mix before and after, how many cases moved and between
/// which dispositions, with a digest that changes when the result does.
/// Why it matters: a changed rule set is activated only with the backtest its approvers saw; the
/// digest ties the approval to that evidence, so a rule change cannot shift the straight-through
/// rate unseen.
#[test]
fn backtest_shows_how_a_rule_change_shifts_outcomes() {
    let current = load_rule_set(
        "f",
        &rules_with("rule('UR-0001', answered_yes('diabetes'), evidence('blood_test'), ['RC-0101'], about = 'blood sugar')"),
    )
    .unwrap();
    let proposed = load_rule_set(
        "f",
        &rules_with("rule('UR-0001', answered_yes('diabetes'), loading(50), ['RC-0101'], about = 'blood sugar')"),
    )
    .unwrap();
    let case = |diabetes: bool| BacktestCase {
        answers: [("diabetes".to_string(), Answer::YesNo(diabetes))].into(),
        age: 40,
        smoker: None,
        cover_type: "life".into(),
        sum_insured_minor: 100_000_00,
        existing_on_life_minor: 0,
    };
    let history = vec![case(true), case(true), case(false)];
    let report = backtest(&current, &proposed, &history);
    assert_eq!(report.cases, 3);
    assert_eq!(
        report.before.by_disposition,
        BTreeMap::from([(Disposition::Clean, 1), (Disposition::NeedsEvidence, 2)])
    );
    assert_eq!(report.after.by_disposition, BTreeMap::from([(Disposition::Clean, 1), (Disposition::Terms, 2)]));
    assert_eq!(report.moved, BTreeMap::from([("needs_evidence>terms".to_string(), 2)]));
    let same = backtest(&current, &current, &history);
    assert!(same.moved.is_empty());
    assert_ne!(report.digest(), same.digest());
    assert_eq!(report.digest(), backtest(&current, &proposed, &history).digest());
    assert!(serde_json::to_string(&report).unwrap().contains("needs_evidence>terms"));
}

/// Purpose: authority profiles with no underwriter, a repeated underwriter, a negative limit or
/// an absurd loading level are refused.
/// Why it matters: a profile is what lets a person bind the insurer; a malformed one must not
/// load as an unlimited one.
#[test]
fn authority_profiles_are_validated_at_load() {
    let p = "profile('u', 1, {'life': 100}, ['term-life'], ['medical'])";
    assert_eq!(load_profiles("f", &format!("PROFILES = [{p}]")).unwrap().len(), 1);
    invalid(load_profiles("f", &format!("PROFILES = [{}]", p.replace("'u'", "''"))), "underwriter");
    invalid(load_profiles("f", &format!("PROFILES = [{p}, {p}]")), "twice");
    invalid(load_profiles("f", &format!("PROFILES = [{}]", p.replace("100", "-1"))), "out of range");
    invalid(
        load_profiles(
            "f",
            &format!("PROFILES = [{}]", p.replace("['medical'])", "['medical'], max_loading_percent = 501)")),
        ),
        "out of range",
    );
}

fn grade(name: &str) -> AuthorityProfile {
    shipped::load().unwrap().grades.into_iter().find(|g| g.underwriter == name).unwrap()
}

fn subject(sum: i64, loading: u32, exclusions: bool, outcome: &'static str) -> Subject<'static> {
    Subject {
        product: "term-life",
        cover_type: "life",
        sum_insured_minor: sum,
        loading_percent: loading,
        has_exclusions: exclusions,
        outcome,
    }
}

/// Purpose: the shipped Cedar policy enforces authority: within limit and product a person may
/// decide; over the limit, on a cover type with no limit, on a product they are not accredited
/// for, with a loading above their level, with an exclusion, decline or deferral they may not
/// give, they may not. Countersigning needs a senior who has not already signed; peer and
/// non-disclosure review need a senior who never touched the case; reassignment needs a lead;
/// a declared conflict forbids everything.
/// Why it matters: authority limits are enforced, not advisory. Each line here is a way a person
/// could otherwise bind the insurer beyond what they are trusted with, or review their own work.
#[test]
fn cedar_policy_enforces_authority_limits() {
    let auth = Authority::shipped().unwrap();
    let clear = Standing::default();
    let (assistant, underwriter, senior) = (grade("grade-assistant"), grade("grade-underwriter"), grade("grade-senior"));
    let may = |p: &AuthorityProfile, a, s: &Subject<'_>, st| auth.permits(p, a, s, st).unwrap();

    assert!(may(&underwriter, CaseAction::Decide, &subject(1_500_000_00, 100, true, "terms"), clear));
    assert!(
        !may(&underwriter, CaseAction::Decide, &subject(1_500_000_01, 0, false, "standard"), clear),
        "over the limit"
    );
    assert!(
        !may(&underwriter, CaseAction::Decide, &subject(1, 101, false, "terms"), clear),
        "loading above level"
    );
    assert!(!may(&assistant, CaseAction::Decide, &subject(1, 0, true, "terms"), clear), "may not exclude");
    assert!(
        !may(&underwriter, CaseAction::Decide, &subject(1, 0, false, "decline"), clear),
        "may not decline"
    );
    assert!(may(&underwriter, CaseAction::Decide, &subject(1, 0, false, "defer"), clear));
    assert!(!may(&assistant, CaseAction::Decide, &subject(1, 0, false, "defer"), clear), "may not defer");
    assert!(may(&senior, CaseAction::Decide, &subject(1, 0, false, "decline"), clear));
    let mut pets = subject(1, 0, false, "standard");
    pets.cover_type = "pets";
    assert!(!may(&senior, CaseAction::Decide, &pets, clear), "no limit for the line");
    let mut other = subject(1, 0, false, "standard");
    other.product = "funeral";
    assert!(!may(&senior, CaseAction::Decide, &other, clear), "not accredited");
    assert!(!may(&senior, CaseAction::Work, &other, clear));
    assert!(
        may(&assistant, CaseAction::Work, &subject(99_000_000_00, 0, false, "none"), clear),
        "work decides nothing"
    );

    assert!(may(&senior, CaseAction::Countersign, &subject(2_000_000_00, 0, false, "decline"), clear));
    assert!(!may(
        &senior,
        CaseAction::Countersign,
        &subject(2_000_000_00, 0, false, "decline"),
        Standing { already_signed: true, ..clear }
    ));
    assert!(
        !may(&underwriter, CaseAction::Countersign, &subject(1, 0, false, "standard"), clear),
        "not senior"
    );
    assert!(
        !may(&senior, CaseAction::Countersign, &subject(5_000_000_01, 0, false, "standard"), clear),
        "over own limit"
    );

    for action in [CaseAction::PeerReview, CaseAction::ReviewNonDisclosure] {
        assert!(may(&senior, action, &subject(1, 0, false, "none"), clear));
        assert!(!may(&senior, action, &subject(1, 0, false, "none"), Standing { touched: true, ..clear }));
        assert!(!may(&underwriter, action, &subject(1, 0, false, "none"), clear));
    }
    assert!(may(&senior, CaseAction::Reassign, &subject(1, 0, false, "none"), clear));
    assert!(may(&grade("grade-chief"), CaseAction::Reassign, &subject(1, 0, false, "none"), clear));
    assert!(!may(&underwriter, CaseAction::Reassign, &subject(1, 0, false, "none"), clear));

    let conflicted = Standing { conflict: true, ..clear };
    for action in [
        CaseAction::Work,
        CaseAction::Decide,
        CaseAction::Countersign,
        CaseAction::PeerReview,
        CaseAction::Reassign,
    ] {
        assert!(!may(&senior, action, &subject(1, 0, false, "standard"), conflicted));
    }
    assert!(matches!(Authority::new("permit (").err(), Some(AuthorityError::Policies(_))));
    // With no policy at all nothing is permitted: the default is deny.
    assert!(!Authority::new("")
        .unwrap()
        .permits(&senior, CaseAction::Work, &subject(1, 0, false, "none"), clear)
        .unwrap());
}

/// Purpose: a case records its handovers, its moves between types and queues, the first touch
/// and the close; an untouched case escalates to the queue lead and then the manager on the
/// timers and never de-escalates; priority rises with size, age, interim cover, chasers and
/// escalation.
/// Why it matters: referral queues are where applications stall. The timers and the priority
/// order are what an applicant with interim cover and a chasing adviser relies on to be seen.
#[test]
fn a_case_tracks_ownership_escalation_and_priority() {
    let timers = Timers::default();
    let mut case = Case::open("c1".into(), CaseType::Referral, Queue::Medical, BTreeSet::from(["RC-0201".to_string()]), t(0));
    assert!(case.is_open());
    assert_eq!(case.escalation_due(&timers, t(7)), Escalation::None);
    assert_eq!(case.escalation_due(&timers, t(8)), Escalation::QueueLead);
    assert_eq!(case.escalation_due(&timers, t(24)), Escalation::Manager);
    let quiet = case.priority(100_000_00, false, t(2));
    case.chasers.push((ChaserFrom::Adviser, t(1)));
    assert_eq!(case.priority(100_000_00, false, t(2)), quiet + 12);
    assert_eq!(case.priority(100_000_00, true, t(2)), quiet + 12 + 24);
    assert_eq!(case.priority(1_000_000_00, false, t(2)), quiet + 12 + 9);
    assert_eq!(case.priority(900_000_000_00, false, t(2)), quiet + 12 + 49, "size is capped");
    case.escalation = Escalation::Manager;
    assert_eq!(case.priority(100_000_00, false, t(2)), quiet + 12 + 96);
    case.escalation = Escalation::QueueLead;
    assert_eq!(case.priority(100_000_00, false, t(2)), quiet + 12 + 48);

    case.assign("amy", t(3));
    case.assign("bo", t(4));
    assert_eq!(case.first_touched_at, Some(t(3)));
    assert_eq!((case.handovers[1].from.as_deref(), case.handovers[1].to.as_str()), (Some("amy"), "bo"));
    assert_eq!(case.touched_by, BTreeSet::from(["amy".to_string(), "bo".to_string()]));
    assert_eq!(case.escalation_due(&timers, t(100)), Escalation::QueueLead, "a touched case keeps its level");

    case.move_to(CaseType::Countersign, Queue::SeniorReview, ["RC-0203".to_string()], t(5));
    assert_eq!(
        (case.owner.clone(), case.queue, case.moves.len(), case.reasons.len()),
        (None, Queue::SeniorReview, 2, 2)
    );
    case.close(t(6));
    case.close(t(7));
    assert_eq!(case.closed_at, Some(t(6)));
    assert!(!case.is_open());
    assert_eq!(
        [
            Queue::Medical,
            Queue::Financial,
            Queue::Occupational,
            Queue::TeleInterview,
            Queue::SeniorReview,
            Queue::ReinsurerLiaison
        ]
        .map(Queue::key),
        ["medical", "financial", "occupational", "tele_interview", "senior_review", "reinsurer_liaison"]
    );
    let mut fresh = Case::open("c2".into(), CaseType::Referral, Queue::Medical, BTreeSet::new(), t(0));
    fresh.escalation = Escalation::Manager;
    assert_eq!(fresh.escalation_due(&timers, t(9)), Escalation::Manager, "never de-escalates");
}

/// Purpose: each kind of evidence goes to the queue whose people read it and has a stable key.
/// Why it matters: financial statements read by a medical underwriter, or the reverse, is a
/// misrouted case; the key is part of the request id that stops a doctor being asked twice.
#[test]
fn evidence_kinds_route_to_the_right_queue() {
    use EvidenceKind::*;
    assert_eq!(FinancialStatement.queue(), Queue::Financial);
    for kind in [GpReport, SpecialistReport, BloodTest, MedicalExam, AlternativeMedicalHistory, PreviousInsurer] {
        assert_eq!(kind.queue(), Queue::Medical);
    }
    assert_eq!(
        [
            GpReport,
            SpecialistReport,
            BloodTest,
            MedicalExam,
            AlternativeMedicalHistory,
            PreviousInsurer,
            FinancialStatement
        ]
        .map(EvidenceKind::key),
        [
            "gp_report",
            "specialist_report",
            "blood_test",
            "medical_exam",
            "alternative_medical_history",
            "previous_insurer",
            "financial_statement"
        ]
    );
}

/// Purpose: for every code in the shipped exclusion library, every cover type and a spread of
/// cause tags (a tag under each entry, its parent, a sibling, every taxonomy root, a malformed
/// tag, the unknown root and no tag at all), the claims module's matcher run on
/// `ExclusionLibrary::for_claims` gives the same outcome as `ExclusionLibrary::matches` for an
/// exclusion in force on the event date; an unknown code goes to an assessor in both; and the
/// two modules agree on the roots of the cause taxonomy.
/// Why it matters: claims cannot import this crate, so it matches with its own code on data
/// produced here. An exclusion that underwriting believes bites and claims does not would pay
/// an excluded claim, and the other way round would decline a covered one. This test is what
/// makes the two matchers one rule.
#[test]
fn claims_and_underwriting_match_exclusions_identically() {
    use ic_core::claims_module::config::{Match, Rules, CAUSE_ROOTS as CLAIMS_ROOTS};
    use ic_underwriting::codes::{CAUSE_ROOTS, COVER_TYPES};

    assert_eq!(CLAIMS_ROOTS, CAUSE_ROOTS);
    let library = shipped::load().unwrap().exclusions;
    let rules = Rules {
        exclusions: library.for_claims(),
        ..Rules::default()
    };
    assert_eq!(rules.exclusions.len(), library.entries.len());

    let mut tags: BTreeSet<String> = CAUSE_ROOTS.iter().map(|r| r.to_string()).collect();
    for entry in library.entries.values() {
        for tag in &entry.cause_tags {
            tags.insert(tag.clone());
            tags.insert(format!("{tag}.deeper"));
            tags.insert(format!("{tag}x"));
            if let Some((parent, _)) = tag.rsplit_once('.') {
                tags.insert(parent.to_string());
            }
        }
    }
    let mut cases: Vec<Vec<String>> = tags.iter().map(|t| vec![t.clone()]).collect();
    cases.push(vec![]);
    cases.push(vec!["Not.A.Tag".to_string()]);
    cases.push(vec!["injury".to_string(), "unknown.cause".to_string()]);
    cases.push(vec!["injury.sport".to_string(), "cancer.skin".to_string()]);

    let on = d(2027, 6, 1);
    let same = |outcome: MatchOutcome| match outcome {
        MatchOutcome::Applies => Match::Applies,
        MatchOutcome::DoesNotApply => Match::DoesNotApply,
        MatchOutcome::NeedsAssessor => Match::NeedsAssessor,
    };
    let mut seen = BTreeSet::new();
    let codes = library.entries.keys().cloned().chain(["EX-0000".to_string()]);
    for code in codes {
        let applied = AppliedExclusion {
            code: code.clone(),
            from: d(2026, 1, 1),
            until: None,
        };
        for cover in COVER_TYPES.iter().chain(&["pets"]) {
            for case in &cases {
                let theirs = rules.exclusion_match(&code, cover, case);
                assert_eq!(theirs, same(library.matches(&applied, cover, case, on)), "{code} on {cover} for {case:?}");
                seen.insert(format!("{theirs:?}"));
            }
        }
    }
    // The spread reaches every outcome, so agreement is not agreement on one answer.
    assert_eq!(seen.len(), 3, "{seen:?}");
}
