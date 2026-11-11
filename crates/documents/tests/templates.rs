//! Templates: submission, approval, rendering tests, activation, disabling.

mod support;

use std::collections::BTreeMap;

use ic_documents::command::{ApprovalRole, Command, Settings};
use ic_documents::state::TemplateStatus;
use ic_documents::template::{AlternateFormat, Part, PartRef, ProductBinding};
use ic_documents::{DocumentKind, Event};
use serde_json::json;
use support::*;

fn submit(kit: &mut Kit, t: ic_documents::template::Template) -> String {
    kit.err("config", "author", Command::SubmitTemplate(Box::new(t)))
}

/// Purpose: a template goes from draft to active only with legal, product and (for a notice)
/// compliance approval and a passing rendering run, and each step is an event.
/// Why it matters: this is the control that stops unreviewed or untested wording reaching a
/// customer. If any gate could be skipped, a wrong notice is one command away.
#[test]
fn a_template_is_approved_tested_and_then_activated() {
    let mut kit = Kit::new();
    let mut t = template("renewal", DocumentKind::RenewalNotice, "Dear {{name}}. You will pay {{premium|money}}.");
    t.minimum_notice_days = Some(30);
    let r = t.reference();
    let events = kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    assert!(matches!(&events[0], Event::TemplateSubmitted { content_hash, .. } if content_hash.len() == 64));
    let today = kit.today();
    let activate = Command::ActivateTemplate {
        template: r.clone(),
        effective_from: today,
    };

    assert_eq!(kit.err("config", "product", activate.clone()), "not_approved");
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product]);
    // A notice is conduct-regulated wording: compliance must approve too.
    assert_eq!(kit.err("config", "product", activate.clone()), "not_approved");
    kit.approve(&r, &[ApprovalRole::Compliance]);
    assert_eq!(kit.err("config", "product", activate.clone()), "not_tested");

    let case = kit.case_for(&t, &sample_data());
    let events = kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: r.clone(),
            cases: vec![case],
        },
    );
    assert_eq!(
        events,
        vec![Event::RenderingTested {
            template: r.clone(),
            cases: 1,
            failures: 0
        }]
    );
    let events = kit.ok("config", "product", activate.clone());
    assert!(matches!(&events[0], Event::TemplateActivated { effective_from, .. } if *effective_from == today));
    assert_eq!(kit.state.templates[&r].status, TemplateStatus::Active);
    assert_eq!(kit.state.template_for("renewal", today).unwrap().template.version, 1);
    assert!(kit.state.template_for("renewal", today.pred_opt().unwrap()).is_none());

    // Activating again with the same date is the same activation.
    assert!(kit.ok("config", "product", activate).is_empty());
    // An active version is immutable: another date, an approval or a test run is refused.
    let later = Command::ActivateTemplate {
        template: r.clone(),
        effective_from: date("2028-01-01"),
    };
    assert_eq!(kit.err("config", "product", later), "config_conflict");
    assert_eq!(
        kit.err(
            "config",
            "legal2",
            Command::ApproveTemplate {
                template: r.clone(),
                role: ApprovalRole::Legal
            }
        ),
        "not_draft"
    );
    let case = kit.case_for(&t, &sample_data());
    assert_eq!(
        kit.err(
            "config",
            "author",
            Command::RunRenderingTests {
                template: r,
                cases: vec![case]
            }
        ),
        "not_draft"
    );
}

/// Purpose: an author cannot approve their own template, one person cannot give two approvals,
/// and a template submitted under someone else's name is refused.
/// Why it matters: the design requires that an author cannot approve. Maker-checker enforced
/// by convention is not a control.
#[test]
fn approvals_are_by_people_other_than_the_author_and_each_other() {
    let mut kit = Kit::new();
    let t = template("receipt", DocumentKind::PremiumReceipt, "We received {{premium|money}}.");
    let r = t.reference();
    assert_eq!(kit.err("config", "someone-else", Command::SubmitTemplate(Box::new(t.clone()))), "invalid");
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t)));
    let approve = |role| Command::ApproveTemplate { template: r.clone(), role };
    assert_eq!(kit.err("config", "author", approve(ApprovalRole::Legal)), "maker_checker");
    kit.ok("config", "lee", approve(ApprovalRole::Legal));
    assert_eq!(kit.err("config", "lee", approve(ApprovalRole::Product)), "maker_checker");
    // The same person repeating their own approval is harmless.
    kit.ok("config", "lee", approve(ApprovalRole::Legal));
    assert_eq!(kit.err("config", "lee", approve_unknown()), "unknown_template");
}

fn approve_unknown() -> Command {
    Command::ApproveTemplate {
        template: tref("nothing", 9),
        role: ApprovalRole::Legal,
    }
}

/// Purpose: a rendering run records each failing case (a field the product does not supply, a
/// render error, output that differs from the expected hash), blocks activation, and counts in
/// the report; a later passing run unblocks it.
/// Why it matters: "a template cannot reference a field a product does not supply" is the test
/// that a rendered document matches its data. A failure must stop activation, not just be logged.
#[test]
fn a_failing_rendering_run_blocks_activation_and_is_reported() {
    let mut kit = Kit::new();
    let t = template("receipt", DocumentKind::PremiumReceipt, "{{name}} paid {{premium|money}}.");
    let r = t.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product]);

    let good = kit.case_for(&t, &sample_data());
    let mut unsupplied = good.clone();
    unsupplied.product_fields = vec!["name".into()];
    let mut wrong_output = good.clone();
    wrong_output.expected_sha256 = "0".repeat(64);
    let mut bad_data = good.clone();
    bad_data.data = json!({"name": "Aroha", "premium": "twelve"});
    let run = |cases| Command::RunRenderingTests { template: r.clone(), cases };
    let events = kit.ok("config", "author", run(vec![good.clone(), unsupplied, wrong_output, bad_data]));
    assert_eq!(
        events,
        vec![Event::RenderingTested {
            template: r.clone(),
            cases: 4,
            failures: 3
        }]
    );
    let failures = &kit.state.templates[&r].last_run.as_ref().unwrap().failures;
    assert!(failures[0].contains("does not supply `premium`"));
    assert!(failures[1].contains("not the expected one"));
    assert!(failures[2].contains("money"));

    let today = kit.today();
    let activate = Command::ActivateTemplate {
        template: r.clone(),
        effective_from: today,
    };
    assert_eq!(kit.err("config", "product", activate.clone()), "tests_failed");
    assert_eq!(kit.state.report(today, today).rendering_test_failures, 1);

    assert_eq!(kit.err("config", "author", run(vec![])), "invalid");
    kit.ok("config", "author", run(vec![good]));
    kit.ok("config", "product", activate);
    assert_eq!(kit.state.report(today, today).active_templates.len(), 1);
}

/// Purpose: activation needs a passing case for every product version the template is bound
/// to and for every language, and for a template bound to no product, one per language.
/// Why it matters: the design asks for a rendering test per template per product version. A
/// template tested on one product and activated for two is untested on the second.
#[test]
fn every_product_version_and_language_needs_a_passing_case() {
    let mut kit = Kit::new();
    let mut t = template("receipt", DocumentKind::PremiumReceipt, "{{name}} paid {{premium|money}}.");
    t.products.push(ProductBinding {
        code: "health".into(),
        version: 2,
    });
    t.bodies.insert("mi".into(), "Kua utu a {{name}} i te {{premium|money}}.".into());
    t.controlling_statements.insert("mi".into(), "Ko te reo Ingarihi te mana.".into());
    let r = t.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product]);
    let today = kit.today();
    let activate = Command::ActivateTemplate {
        template: r.clone(),
        effective_from: today,
    };

    let en = kit.case_for(&t, &sample_data());
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: r.clone(),
            cases: vec![en.clone()],
        },
    );
    assert_eq!(kit.err("config", "product", activate.clone()), "not_tested");

    let mut cases = vec![];
    for (code, version) in [("term-life", 1), ("health", 2)] {
        for language in ["en", "mi"] {
            let mut case = en.clone();
            case.product_code = code.into();
            case.product_version = version;
            case.language = language.into();
            let text = t.render(&kit.state.parts, language, &case.data).unwrap();
            if language == "mi" {
                // A translation says which language controls.
                assert!(text.ends_with("Ko te reo Ingarihi te mana."));
            }
            case.expected_sha256 = ic_documents::template::sha256_hex(text.as_bytes());
            cases.push(case);
        }
    }
    kit.ok("config", "author", Command::RunRenderingTests { template: r.clone(), cases });
    kit.ok("config", "product", activate);

    // Bound to no product: one passing case per language is enough.
    let mut outcome = template("outcome", DocumentKind::GeneralCorrespondence, "Dear {{name}}.");
    outcome.products.clear();
    let o = outcome.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(outcome.clone())));
    kit.approve(&o, &[ApprovalRole::Legal, ApprovalRole::Product]);
    let mut case = kit.case_for(&outcome, &sample_data());
    case.language = "en".into();
    let mut failing = case.clone();
    failing.expected_sha256 = "f".repeat(64);
    let activate = Command::ActivateTemplate {
        template: o.clone(),
        effective_from: today,
    };
    assert_eq!(kit.err("config", "product", activate.clone()), "not_tested");
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: o.clone(),
            cases: vec![case],
        },
    );
    kit.ok("config", "product", activate);
}

/// Purpose: submission refuses a template that fails any structural rule: no id, no title, no
/// alternate format, a notice period on a document that is not proven, wording above the
/// reading-level limit, a translation that uses other fields or does not say which language
/// controls, an undeclared or unregistered part, and malformed syntax.
/// Why it matters: plain language, accessibility and translation consistency are requirements
/// of the template in the design, checked before a person reviews it.
#[test]
fn submission_checks_structure_readability_and_translations() {
    let mut kit = Kit::new();
    let base = || template("t", DocumentKind::PremiumReceipt, "We received {{premium|money}}.");

    let mut t = base();
    t.id.clear();
    assert_eq!(submit(&mut kit, t), "invalid_template");
    let mut t = base();
    t.title.clear();
    assert_eq!(submit(&mut kit, t), "invalid_template");
    let mut t = base();
    t.alternate_formats.clear();
    assert_eq!(submit(&mut kit, t), "invalid_template");
    let mut t = base();
    t.minimum_notice_days = Some(30);
    assert_eq!(submit(&mut kit, t), "invalid_template");
    let mut t = base();
    t.primary_language = "fr".into();
    assert_eq!(submit(&mut kit, t), "invalid_template");

    let hard = "Notwithstanding antecedent contractual representations, indemnification obligations \
                remain unconditionally enforceable irrespective of subsequent circumstantial modifications.";
    assert_eq!(submit(&mut kit, template("t", DocumentKind::PremiumReceipt, hard)), "invalid_template");
    // The limit is a setting: with a higher one the same wording passes.
    kit.ok(
        "config",
        "ops",
        Command::ActivateSettings(Settings {
            version: 1,
            max_reading_grade_x10: 400,
            ..Settings::default()
        }),
    );
    kit.ok(
        "config",
        "author",
        Command::SubmitTemplate(Box::new(template("hard", DocumentKind::PremiumReceipt, hard))),
    );

    let mut t = base();
    t.bodies.insert("mi".into(), "Kua tae mai te {{amount|money}}.".into());
    t.controlling_statements.insert("mi".into(), "English controls.".into());
    assert_eq!(submit(&mut kit, t), "invalid_template");
    let mut t = base();
    t.bodies.insert("mi".into(), "Kua tae mai te {{premium|money}}.".into());
    assert_eq!(submit(&mut kit, t), "invalid_template");

    assert_eq!(
        submit(&mut kit, template("t", DocumentKind::PremiumReceipt, "{{> clause}}")),
        "invalid_template"
    );
    let mut t = template("t", DocumentKind::PremiumReceipt, "{{> clause}}");
    t.parts.push(PartRef {
        id: "clause".into(),
        version: 1,
    });
    assert_eq!(submit(&mut kit, t), "invalid_template");
    assert_eq!(
        submit(&mut kit, template("t", DocumentKind::PremiumReceipt, "{{#if x}} never closed")),
        "invalid_template"
    );
    assert!(kit.state.templates.len() == 1);
}

/// Purpose: a part is registered once per version, a template composed from it includes its
/// text in the right language, and the template's content hash changes when the part does.
/// Why it matters: a clause used by ten products is one artefact. The hash must cover the
/// parts, or two different wordings could carry the same version hash.
#[test]
fn a_template_is_composed_from_parts_and_hashed_with_them() {
    let mut kit = Kit::new();
    let part = |version: u32, text: &str| Part {
        id: "cooling-off".into(),
        version,
        bodies: BTreeMap::from([("en".to_string(), text.to_string())]),
    };
    let events = kit.ok("config", "author", Command::RegisterPart(part(1, "You may cancel within {{days}} days.")));
    assert!(matches!(&events[0], Event::PartRegistered { version: 1, .. }));
    assert!(kit
        .ok("config", "author", Command::RegisterPart(part(1, "You may cancel within {{days}} days.")))
        .is_empty());
    assert_eq!(kit.err("config", "author", Command::RegisterPart(part(1, "Other words."))), "config_conflict");
    assert_eq!(kit.err("config", "author", Command::RegisterPart(part(0, "x"))), "invalid");
    kit.ok("config", "author", Command::RegisterPart(part(2, "You have {{days}} days to cancel.")));

    let mut t = template("wording", DocumentKind::PolicyWording, "Welcome {{name}}. {{> cooling-off}}");
    t.parts.push(PartRef {
        id: "cooling-off".into(),
        version: 1,
    });
    t.bodies.insert("mi".into(), "Nau mai {{name}}. {{> cooling-off}}".into());
    t.controlling_statements.insert("mi".into(), "English controls.".into());
    let data = json!({"name": "Aroha", "days": 14});
    assert_eq!(
        t.render(&kit.state.parts, "en", &data).unwrap(),
        "Welcome Aroha. You may cancel within 14 days."
    );
    // The part has no Māori version, so the primary language's text is used.
    assert!(t
        .render(&kit.state.parts, "mi", &data)
        .unwrap()
        .starts_with("Nau mai Aroha. You may cancel within 14 days."));
    assert!(t.render(&kit.state.parts, "fr", &data).is_err());
    let v1 = t.content_hash(&kit.state.parts);
    t.parts[0].version = 2;
    assert_ne!(t.content_hash(&kit.state.parts), v1);
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone())));
    // The same content again is the same submission; other content under the version is refused.
    assert!(kit.ok("config", "author", Command::SubmitTemplate(Box::new(t.clone()))).is_empty());
    t.title = "Changed".into();
    assert_eq!(kit.err("config", "author", Command::SubmitTemplate(Box::new(t))), "config_conflict");
}

/// Purpose: disabling an active template stops anything more being issued from it, needs a
/// reason and a case, is idempotent, and a draft cannot be disabled.
/// Why it matters: runbook eight's containment step is "disable the template version". After
/// it, not one more wrong notice may go out.
#[test]
fn a_disabled_template_issues_nothing_more() {
    let mut kit = Kit::ready();
    let r = tref("renewal", 1);
    let disable = |reason: &str| Command::DisableTemplate {
        template: r.clone(),
        reason: reason.into(),
        case: "INC-8".into(),
    };
    assert_eq!(kit.err("remediation", "commander", disable("")), "invalid");
    let events = kit.ok("remediation", "commander", disable("wrong premium figure"));
    assert_eq!(
        events,
        vec![Event::TemplateDisabled {
            template: r.clone(),
            case: "INC-8".into()
        }]
    );
    assert!(kit.ok("remediation", "commander", disable("again")).is_empty());
    let spec = kit.renewal_spec("n1");
    assert_eq!(kit.issue_err(spec), "template_disabled");
    assert!(kit.state.template_for("renewal", kit.today()).is_none());
    assert_eq!(
        kit.err(
            "config",
            "product",
            Command::ActivateTemplate {
                template: r,
                effective_from: kit.today()
            }
        ),
        "config_conflict"
    );

    kit.ok(
        "config",
        "author",
        Command::SubmitTemplate(Box::new(template("draft", DocumentKind::Reminder, "Hello {{name}}."))),
    );
    assert_eq!(
        kit.err(
            "config",
            "ops",
            Command::DisableTemplate {
                template: tref("draft", 1),
                reason: "r".into(),
                case: "c".into()
            }
        ),
        "not_active"
    );
}

/// Purpose: settings and retention schedules are versioned and immutable: the same version
/// with the same content is accepted, other content or an older version is refused, and a
/// retention schedule must cover every document kind.
/// Why it matters: each document's retention and each delivery's timing must be explainable
/// from the configuration version in force. Editing a version in place would destroy that.
#[test]
fn settings_and_retention_versions_are_immutable() {
    use ic_documents::model::{RetentionBasis, RetentionRule};
    let mut kit = Kit::new();
    let v2 = Settings {
        version: 2,
        post_deemed_days: 4,
        ..Settings::default()
    };
    assert_eq!(kit.err("config", "ops", Command::ActivateSettings(Settings::default())), "invalid");
    assert_eq!(
        kit.ok("config", "ops", Command::ActivateSettings(v2.clone())),
        vec![Event::SettingsActivated { version: 2 }]
    );
    assert!(kit.ok("config", "ops", Command::ActivateSettings(v2.clone())).is_empty());
    assert_eq!(
        kit.err("config", "ops", Command::ActivateSettings(Settings { post_deemed_days: 9, ..v2 })),
        "config_conflict"
    );
    assert_eq!(
        kit.err(
            "config",
            "ops",
            Command::ActivateSettings(Settings {
                version: 1,
                ..Settings::default()
            })
        ),
        "config_conflict"
    );
    assert_eq!(kit.state.settings.post_deemed_days, 4);

    let rule = RetentionRule {
        basis: RetentionBasis::Issue,
        days: 10,
    };
    let all: BTreeMap<_, _> = DocumentKind::ALL.iter().map(|k| (*k, rule)).collect();
    let mut partial = all.clone();
    partial.remove(&DocumentKind::Invoice);
    assert_eq!(kit.err("config", "ops", Command::ActivateRetention { version: 1, rules: partial }), "invalid");
    assert_eq!(
        kit.ok(
            "config",
            "ops",
            Command::ActivateRetention {
                version: 1,
                rules: all.clone()
            }
        ),
        vec![Event::RetentionActivated { version: 1 }]
    );
    assert!(kit
        .ok(
            "config",
            "ops",
            Command::ActivateRetention {
                version: 1,
                rules: all.clone()
            }
        )
        .is_empty());
    let mut other = all.clone();
    other.insert(
        DocumentKind::Invoice,
        RetentionRule {
            basis: RetentionBasis::Issue,
            days: 99,
        },
    );
    assert_eq!(
        kit.err(
            "config",
            "ops",
            Command::ActivateRetention {
                version: 1,
                rules: other.clone()
            }
        ),
        "config_conflict"
    );
    kit.ok(
        "config",
        "ops",
        Command::ActivateRetention {
            version: 3,
            rules: other.clone(),
        },
    );
    assert_eq!(
        kit.err("config", "ops", Command::ActivateRetention { version: 2, rules: other }),
        "config_conflict"
    );
    assert_eq!(kit.state.retention_version, 3);
}

/// Purpose: a newer template version takes over from its effective date, and the report lists
/// active templates and those due for review.
/// Why it matters: a notice records the template version that rendered it. Selection by
/// effective date must be exact, or a document is rendered with wording not yet in force.
#[test]
fn a_new_version_takes_over_from_its_effective_date() {
    let mut kit = Kit::ready();
    let mut v2 = template("receipt", DocumentKind::PremiumReceipt, "Thank you. We received {{premium|money}}.");
    v2.version = 2;
    v2.review_by = date("2027-12-01");
    let r = v2.reference();
    kit.ok("config", "author", Command::SubmitTemplate(Box::new(v2.clone())));
    kit.approve(&r, &[ApprovalRole::Legal, ApprovalRole::Product]);
    let case = kit.case_for(&v2, &sample_data());
    kit.ok(
        "config",
        "author",
        Command::RunRenderingTests {
            template: r.clone(),
            cases: vec![case],
        },
    );
    kit.ok(
        "config",
        "product",
        Command::ActivateTemplate {
            template: r.clone(),
            effective_from: date("2027-12-01"),
        },
    );
    assert_eq!(kit.state.template_for("receipt", date("2027-11-30")).unwrap().template.version, 1);
    assert_eq!(kit.state.template_for("receipt", date("2027-12-01")).unwrap().template.version, 2);

    // Today version 1 is current: a document rendered with version 2 is refused.
    let mut spec = kit.issue_spec("r1", DocumentKind::PremiumReceipt, "receipt");
    spec.template = r.clone();
    assert_eq!(kit.issue_err(spec), "template_not_current");
    let report = kit.state.report(date("2027-11-01"), date("2027-12-31"));
    assert_eq!(report.active_templates.len(), 7);
    assert_eq!(report.due_for_review, vec![r]);
    assert!(AlternateFormat::LargePrint < AlternateFormat::Braille);
}
