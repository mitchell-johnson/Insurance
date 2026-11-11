//! The envelope, configuration, direct debit authorities and payee accounts.

mod common;

use common::*;
use ic_payments::model::{AuthorityStatus, NameCheck, PayeeKind, RunType};
use ic_payments::{Command, Event, Role};

/// Purpose: a command repeated with the same key is answered with the first
/// answer and applied once.
/// Why it matters: delivery to the module is at least once. A retry after a
/// timeout must not register a payee, or approve a run, a second time.
#[test]
fn a_repeated_command_is_answered_once() {
    let mut desk = Desk::new();
    let envelope = desk.envelope(
        &pam(),
        Command::RegisterPayee {
            payee: "p1".to_string(),
            party: "party".to_string(),
            kind: PayeeKind::Customer,
            account: account("Ana", "0101234567"),
            name_check: NameCheck::Match,
        },
    );
    use ic_log::StateMachine;
    let first = desk.state.apply(100, envelope.clone());
    let seq = desk.state.event_seq;
    let again = desk.state.apply(101, envelope);
    assert_eq!(first, again);
    assert!(first.is_ok());
    assert_eq!(desk.state.event_seq, seq, "the repeat produced no new events");
}

/// Purpose: an envelope without a key or a sender is refused, and so is a
/// sender who is both an approver and a maintainer of payees or connectivity.
/// Why it matters: the person who releases money must never be able to decide
/// who is paid or where the file goes. That separation is the main defence
/// against payment fraud from inside.
#[test]
fn an_approver_cannot_also_maintain_payees_or_connectivity() {
    let mut desk = Desk::new();
    let both = person("eve", &[Role::Approver, Role::PayeeMaintainer]);
    assert_eq!(desk.code(&both, Command::ClosePayee { payee: "p".to_string() }), "role_conflict");
    let both = person("eve", &[Role::Approver, Role::ConnectivityAdmin]);
    assert_eq!(desk.code(&both, Command::ClosePayee { payee: "p".to_string() }), "role_conflict");
    assert_eq!(
        desk.code(&person("", &[Role::Maker]), Command::CloseRun { run: "r".to_string() }),
        "invalid_envelope"
    );
    let mut envelope = desk.envelope(&mia(), Command::CloseRun { run: "r".to_string() });
    envelope.key = String::new();
    use ic_log::StateMachine;
    assert_eq!(desk.state.apply(1, envelope).unwrap_err().code, "invalid_envelope");
}

/// Purpose: time in the module never runs backwards, whatever a late or
/// skewed proposer says.
/// Why it matters: cooling-off periods and bank cutoffs are measured on the
/// module's clock. A command stamped in the past must not reopen a cutoff or
/// shorten a cooling-off period.
#[test]
fn the_clock_never_runs_backwards() {
    let mut desk = Desk::new();
    desk.at(t(2027, 11, 16, 12, 0));
    desk.ok(&svc(), Command::Prune { before: d(2020, 1, 1) });
    desk.at(t(2027, 11, 15, 8, 0));
    desk.ok(&svc(), Command::Prune { before: d(2020, 1, 1) });
    assert_eq!(desk.state.clock, Some(t(2027, 11, 16, 12, 0)));
}

/// Purpose: configuration versions rise by one, an active version cannot be
/// changed, and only a connectivity administrator activates one.
/// Why it matters: bank accounts, limits and routes are the controls
/// themselves. They change by a new, recorded version and never in place.
#[test]
fn configuration_is_versioned_and_immutable() {
    let mut desk = Desk::bare();
    let activate = |version, config| Command::ActivateConfig {
        version,
        config: Box::new(config),
    };
    assert_eq!(desk.code(&mia(), Command::CloseRun { run: "r".to_string() }), "unknown_run");
    assert_eq!(
        desk.code(
            &pam(),
            Command::ChangePayeeAccount {
                payee: "p".to_string(),
                account: account("A", "0101234567"),
                name_check: NameCheck::Match
            }
        ),
        "not_configured"
    );
    assert_eq!(desk.code(&mia(), activate(1, config())), "not_permitted");
    assert_eq!(desk.code(&cal(), activate(2, config())), "config_gap");
    let events = desk.ok(&cal(), activate(1, config()));
    assert_eq!(events, vec![Event::ConfigActivated { version: 1 }]);
    // The same version with the same content is a repeat, not a change.
    assert!(desk.ok(&cal(), activate(1, config())).is_empty());
    let mut other = config();
    other.rules.cooling_off_hours = 1;
    assert_eq!(desk.code(&cal(), activate(1, other.clone())), "config_conflict");
    desk.ok(&cal(), activate(2, other));
    assert_eq!(desk.state.config_version, 2);
    assert_eq!(desk.state.config.as_ref().unwrap().rules.cooling_off_hours, 1);
}

/// Purpose: a configuration is refused when it is not internally whole: an
/// unknown bank, a bad account code, a route with no account for a fund,
/// limits that do not nest, a second "bank" that is the first, an urgent
/// channel that is not an API.
/// Why it matters: a hole in configuration shows up as a run that cannot be
/// paid on the day it is due. It is found at activation instead.
#[test]
fn a_configuration_with_holes_is_refused() {
    let refused = |change: fn(&mut ic_payments::Config)| {
        let mut desk = Desk::bare();
        let mut c = config();
        change(&mut c);
        desk.code(
            &cal(),
            Command::ActivateConfig {
                version: 1,
                config: Box::new(c),
            },
        )
    };
    assert_eq!(refused(|c| c.rules.voter_regions.clear()), "invalid_config");
    assert_eq!(refused(|c| c.funds.clear()), "invalid_config");
    assert_eq!(refused(|c| c.channels.get_mut("anz-api").unwrap().bank = "kiwi".to_string()), "invalid_config");
    assert_eq!(
        refused(|c| {
            let a = c.accounts.remove("anz-sf1-claims").unwrap();
            c.accounts.insert("anz sf1/claims".to_string(), a);
        }),
        "invalid_config"
    );
    assert_eq!(
        refused(|c| c.accounts.get_mut("anz-sf1-claims").unwrap().fund = "sf9".to_string()),
        "invalid_config"
    );
    assert_eq!(refused(|c| c.routes.get_mut(&RunType::Commission).unwrap().approvals = 0), "invalid_config");
    assert_eq!(
        refused(|c| c.routes.get_mut(&RunType::Commission).unwrap().run_limit_minor = 1),
        "invalid_config"
    );
    assert_eq!(
        refused(|c| c.routes.get_mut(&RunType::Commission).unwrap().primary = "nowhere".to_string()),
        "invalid_config"
    );
    assert_eq!(
        refused(|c| c.routes.get_mut(&RunType::Commission).unwrap().urgent = Some("bnz-file".to_string())),
        "invalid_config"
    );
    assert_eq!(
        refused(|c| c.routes.get_mut(&RunType::Commission).unwrap().secondary = Some("anz-api".to_string())),
        "invalid_config"
    );
    assert_eq!(refused(|c| drop(c.accounts.remove("bnz-sf1-refunds"))), "invalid_config");
}

/// Purpose: a configuration that leaves any voter region unable to reach any
/// bank channel is refused: no endpoint, an address the bank has not
/// whitelisted, or an expired certificate.
/// Why it matters: after a region is lost, the region that holds the leaders
/// must be able to submit a direct debit file and an urgent payment at once.
/// Finding out on the day that Sydney was never whitelisted means premiums
/// are not collected and claims are not paid.
#[test]
fn every_voter_region_must_be_able_to_reach_every_channel() {
    let refused = |change: fn(&mut ic_payments::Config)| {
        let mut desk = Desk::bare();
        let mut c = config();
        change(&mut c);
        desk.code(
            &cal(),
            Command::ActivateConfig {
                version: 1,
                config: Box::new(c),
            },
        )
    };
    assert_eq!(
        refused(|c| drop(c.channels.get_mut("anz-file").unwrap().endpoints.remove("mel"))),
        "region_not_provisioned"
    );
    assert_eq!(
        refused(|c| c.channels.get_mut("bnz-file").unwrap().endpoints.get_mut("syd").unwrap().whitelisted = false),
        "region_not_provisioned"
    );
    assert_eq!(
        refused(|c| c.channels.get_mut("anz-api").unwrap().endpoints.get_mut("akl").unwrap().certificate_expires = d(2027, 11, 15)),
        "region_not_provisioned"
    );
    let gaps = {
        let mut c = config();
        c.channels.get_mut("anz-file").unwrap().endpoints.remove("mel");
        c.provisioning_gaps(d(2028, 7, 1))
    };
    assert!(gaps.iter().any(|g| g.contains("no endpoint in mel")));
    assert!(gaps.iter().any(|g| g.contains("certificate in syd has expired")));
}

/// Purpose: an authority is created, suspended, reinstated and cancelled,
/// each only from the state that allows it and only by a payee maintainer.
/// Why it matters: collecting on a cancelled authority is an unauthorised
/// debit of a customer's account, which the bank reverses and the regulator
/// hears about.
#[test]
fn an_authority_moves_through_its_states() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Active);
    let create = |number: &str| Command::CreateAuthority {
        authority: "a1".to_string(),
        policy: policy("pol-1"),
        party: "x".to_string(),
        account: account("Ana", number),
        sole_name: false,
    };
    assert_eq!(desk.code(&pam(), create("0101234567")), "duplicate_authority");
    assert_eq!(desk.code(&pam(), create("12")), "invalid_account");
    assert_eq!(desk.code(&pam(), create("01-0123-45x7")), "invalid_account");
    assert_eq!(desk.code(&mia(), create("0101234567")), "not_permitted");
    let a = || "a1".to_string();
    assert_eq!(desk.code(&pam(), Command::ReinstateAuthority { authority: a() }), "authority_not_suspended");
    assert_eq!(
        desk.code(
            &pam(),
            Command::SuspendAuthority {
                authority: "zz".to_string(),
                reason: "r".to_string()
            }
        ),
        "unknown_authority"
    );
    desk.ok(
        &pam(),
        Command::SuspendAuthority {
            authority: a(),
            reason: "customer asked".to_string(),
        },
    );
    assert_eq!(
        desk.code(
            &pam(),
            Command::SuspendAuthority {
                authority: a(),
                reason: "again".to_string()
            }
        ),
        "authority_not_active"
    );
    desk.ok(&pam(), Command::ReinstateAuthority { authority: a() });
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Active);
    assert_eq!(desk.state.authorities["a1"].reason, None);
    desk.ok(
        &pam(),
        Command::CancelAuthority {
            authority: a(),
            reason: "closed".to_string(),
        },
    );
    assert_eq!(
        desk.code(
            &pam(),
            Command::CancelAuthority {
                authority: a(),
                reason: "closed".to_string()
            }
        ),
        "authority_cancelled"
    );
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Cancelled);
}

/// Purpose: when the core reports that a policy's payer has died, the
/// authority on that policy is cancelled, an account in the deceased's sole
/// name is blocked, every other authority drawing on that account stops with
/// a notice to arrange another way to pay, and the account cannot be used
/// for a new authority.
/// Why it matters: collecting premium from a dead person's account is
/// exactly the harm the late-notified death rules exist to prevent, and the
/// core only knows about the one policy the claim is on.
#[test]
fn a_deceased_payers_account_is_never_drawn_on_again() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", true);
    desk.authority("a2", "pol-2", "0101234567", true);
    desk.authority("a3", "pol-3", "0207654321", false);
    let death = Command::PayerDeceased {
        policy: policy("pol-1"),
        claim: "clm-1".to_string(),
    };
    assert_eq!(desk.code(&mia(), death.clone()), "not_permitted");
    let events = desk.ok(&svc(), death);
    assert!(events.contains(&Event::AccountBlocked { authority: "a1".to_string() }));
    assert_eq!(desk.state.authorities["a1"].reason.as_deref(), Some("payer_deceased"));
    assert_eq!(desk.state.authorities["a2"].reason.as_deref(), Some("account_holder_deceased"));
    assert_eq!(desk.state.authorities["a3"].status, AuthorityStatus::Active);
    let notices: Vec<_> = desk.state.outbox.values().map(|e| e.key.clone()).collect();
    assert_eq!(notices, vec!["deceased-clm-1-a2"]);
    let again = Command::CreateAuthority {
        authority: "a4".to_string(),
        policy: policy("pol-4"),
        party: "x".to_string(),
        account: account("Ana", "0101234567"),
        sole_name: false,
    };
    assert_eq!(desk.code(&pam(), again), "account_blocked");
}

/// Purpose: a joint account is not blocked when one payer dies: the
/// authority on the policy is cancelled and nothing else.
/// Why it matters: the surviving account holder still owns the account and
/// may be paying for their own cover from it.
#[test]
fn a_joint_account_is_not_blocked_by_one_death() {
    let mut desk = Desk::new();
    desk.authority("a1", "pol-1", "0101234567", false);
    desk.authority("a2", "pol-2", "0101234567", false);
    desk.ok(
        &svc(),
        Command::PayerDeceased {
            policy: policy("pol-1"),
            claim: "clm-1".to_string(),
        },
    );
    assert_eq!(desk.state.authorities["a1"].status, AuthorityStatus::Cancelled);
    assert_eq!(desk.state.authorities["a2"].status, AuthorityStatus::Active);
    assert!(desk.state.blocked_accounts.is_empty());
}

/// Purpose: a payee cannot be paid until the bank's name check has not
/// failed, a second person has confirmed the details, and screening is
/// clear; the person who entered the details cannot confirm them.
/// Why it matters: redirecting a payment to an account the fraudster
/// controls is the commonest payment fraud. Each control closes one way in.
#[test]
fn a_payee_passes_every_control_before_it_can_be_paid() {
    let mut desk = Desk::new();
    let register = |id: &str, check| Command::RegisterPayee {
        payee: id.to_string(),
        party: "party".to_string(),
        kind: PayeeKind::Customer,
        account: account("Ana", "0101234567"),
        name_check: check,
    };
    let p = || "p1".to_string();
    assert_eq!(desk.code(&mia(), register("p1", NameCheck::Match)), "not_permitted");
    desk.ok(&pam(), register("p1", NameCheck::Close));
    assert_eq!(desk.code(&pam(), register("p1", NameCheck::Match)), "duplicate_payee");
    let blocked = |desk: &Desk| desk.state.payees["p1"].blocked(desk.now);
    assert_eq!(blocked(&desk), Some("payee_not_confirmed"));
    assert_eq!(desk.code(&pam(), Command::ConfirmPayee { payee: p() }), "same_person");
    assert_eq!(desk.code(&pat(), Command::ConfirmPayee { payee: "zz".to_string() }), "unknown_payee");
    desk.ok(&pat(), Command::ConfirmPayee { payee: p() });
    assert_eq!(blocked(&desk), Some("payee_not_screened"));
    assert_eq!(desk.code(&pat(), Command::ScreenPayee { payee: p(), clear: true }), "not_permitted");
    desk.ok(&svc(), Command::ScreenPayee { payee: p(), clear: false });
    assert_eq!(blocked(&desk), Some("payee_not_screened"));
    desk.ok(&svc(), Command::ScreenPayee { payee: p(), clear: true });
    assert_eq!(blocked(&desk), None);
    assert_eq!(desk.code(&lee(), Command::ApprovePayee { payee: p() }), "approval_not_needed");

    desk.ok(&pam(), register("p2", NameCheck::NoMatch));
    assert_eq!(desk.state.payees["p2"].blocked(desk.now), Some("payee_name_mismatch"));
    assert_eq!(desk.code(&pat(), Command::ConfirmPayee { payee: "p2".to_string() }), "payee_name_mismatch");

    desk.ok(&pam(), Command::ClosePayee { payee: p() });
    assert_eq!(blocked(&desk), Some("payee_closed"));
    assert_eq!(desk.code(&pat(), Command::ConfirmPayee { payee: p() }), "payee_closed");
}

/// Purpose: changed account details reset every control and cannot be paid
/// until the cooling-off period has passed.
/// Why it matters: a change of bank account shortly before a payment is the
/// signature of account takeover. The wait gives the real payee time to
/// notice the confirmation they did not ask for.
#[test]
fn changed_details_wait_out_the_cooling_off_period() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    assert_eq!(desk.state.payees["p1"].blocked(desk.now), None);
    let change = |number: &str| Command::ChangePayeeAccount {
        payee: "p1".to_string(),
        account: account("Ana", number),
        name_check: NameCheck::Match,
    };
    assert_eq!(desk.code(&pam(), change("1")), "invalid_account");
    assert_eq!(desk.code(&mia(), change("0209999999")), "not_permitted");
    let events = desk.ok(&pam(), change("0209999999"));
    let until = t(2027, 11, 16, 9, 0);
    assert_eq!(
        events,
        vec![Event::PayeeAccountChanged {
            payee: "p1".to_string(),
            cooling_off_until: until
        }]
    );
    desk.ok(&pat(), Command::ConfirmPayee { payee: "p1".to_string() });
    desk.ok(
        &svc(),
        Command::ScreenPayee {
            payee: "p1".to_string(),
            clear: true,
        },
    );
    assert_eq!(desk.state.payees["p1"].blocked(desk.now), Some("payee_cooling_off"));
    assert_eq!(desk.state.payees["p1"].blocked(until), None);
}

/// Purpose: an account for an executor or a liquidator needs legal's
/// approval, from someone other than the person who entered it, and waits
/// out the cooling-off period even though it is new.
/// Why it matters: paying an estate or a failed adviser's held balance to
/// the wrong person cannot be recovered from the right one. The addendum
/// makes legal's approval and the standard verification unconditional here.
#[test]
fn an_estate_or_liquidator_account_needs_legal_and_always_cools_off() {
    let mut desk = Desk::new();
    for (id, kind) in [("estate", PayeeKind::Estate), ("liq", PayeeKind::Liquidator)] {
        desk.ok(
            &pam(),
            Command::RegisterPayee {
                payee: id.to_string(),
                party: "party".to_string(),
                kind,
                account: account("Executor", "0301234567"),
                name_check: NameCheck::Match,
            },
        );
        desk.ok(&pat(), Command::ConfirmPayee { payee: id.to_string() });
        desk.ok(
            &svc(),
            Command::ScreenPayee {
                payee: id.to_string(),
                clear: true,
            },
        );
        assert_eq!(desk.state.payees[id].blocked(desk.now), Some("payee_needs_legal_approval"));
        assert_eq!(desk.code(&pam(), Command::ApprovePayee { payee: id.to_string() }), "not_permitted");
        let both = person("pam", &[Role::Legal]);
        assert_eq!(desk.code(&both, Command::ApprovePayee { payee: id.to_string() }), "same_person");
        desk.ok(&lee(), Command::ApprovePayee { payee: id.to_string() });
        assert_eq!(desk.state.payees[id].blocked(desk.now), Some("payee_cooling_off"));
        assert_eq!(desk.state.payees[id].blocked(desk.now + chrono::Duration::hours(24)), None);
    }
}
