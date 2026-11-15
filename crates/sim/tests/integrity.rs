//! Tests that the safety nets themselves work: reconciliation notices each
//! kind of damage, the chart of accounts is frozen, and every command is
//! owned by a module.

use chrono::NaiveDate;
use ic_core::accounts::{self, role};
use ic_core::recon::{control_totals, reconcile};
use ic_core::{quote, Command, CoreState, GroupId, LifeId, PartyId, PolicyId, PolicyStatus};
use ic_ledger::{AccountClass, AccountUnit};
use ic_money::{Currency::NZD, FundId, Money};
use ic_product::{load_product, Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{group_scheme, issue_command, member, term_life};
use ic_sim::Cluster;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

const MONTHLY: i64 = 61_84;

/// One partition with one standard policy, paid, ten days in.
fn book() -> (Cluster, PolicyId) {
    let mut c = Cluster::in_memory(1, 42);
    c.open(d(2027, 11, 15)).unwrap();
    for cmd in shipped_activations().unwrap() {
        c.activate_everywhere(cmd).unwrap();
    }
    let issue = term_life(
        1,
        d(2027, 11, 15),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "r1".into(),
            amount: Money::new(MONTHLY + 500, NZD),
        },
    )
    .unwrap();
    for _ in 0..10 {
        assert!(c.end_of_day().unwrap().breaks.is_empty());
    }
    (c, id)
}

fn state(c: &mut Cluster) -> &mut CoreState {
    &mut c.nodes.get_mut(&0).unwrap().replica.state
}

fn checks(c: &Cluster) -> Vec<&'static str> {
    c.reconcile().iter().map(|b| b.check).collect()
}

/// Purpose: reconciliation reports a break when a policy's invoices no longer agree with
/// its receivable account in the ledger.
/// Why it matters: the invoice list and the ledger are two records of the same debt. If a
/// bug ever updates one without the other, this check is the only thing that notices
/// before a customer is chased for the wrong amount.
#[test]
fn reconciliation_notices_invoices_that_disagree_with_the_ledger() {
    let (mut c, id) = book();
    assert!(checks(&c).is_empty(), "the untouched book is clean");
    state(&mut c).policies.get_mut(&id).unwrap().receivables[0].outstanding_minor += 1;
    let found = checks(&c);
    assert!(found.contains(&"premium_receivable"), "{found:?}");
    assert!(found.contains(&"receivable_control"), "the control total disagrees too: {found:?}");
}

/// Purpose: reconciliation reports a break when the earned amounts recorded on a policy
/// no longer agree with its unearned premium account.
/// Why it matters: unearned premium is a liability on the insurer's balance sheet. A
/// drift here misstates profit, and it compounds every day the earning run repeats it.
#[test]
fn reconciliation_notices_earning_that_disagrees_with_the_ledger() {
    let (mut c, id) = book();
    let r = &mut state(&mut c).policies.get_mut(&id).unwrap().receivables[0];
    *r.earned_fine.values_mut().next().unwrap() += 1;
    let found = checks(&c);
    assert!(found.contains(&"unearned_premium") && found.contains(&"unearned_control"), "{found:?}");
}

/// Purpose: reconciliation reports a policy whose standing does not fit its status: a
/// lapsed policy with a standing, or a policy in force with none.
/// Why it matters: standing drives arrears letters and claims eligibility. The rule that
/// only a policy in force has a standing is what keeps a lapsed customer out of the
/// collections queue.
#[test]
fn reconciliation_notices_a_standing_that_does_not_fit_the_status() {
    let (mut c, id) = book();
    state(&mut c).policies.get_mut(&id).unwrap().status = PolicyStatus::Lapsed;
    assert!(checks(&c).contains(&"standing"));
    let p = state(&mut c).policies.get_mut(&id).unwrap();
    p.status = PolicyStatus::InForce;
    p.standing = None;
    assert!(checks(&c).contains(&"standing"));
}

/// Purpose: reconciliation reports a policy removed from state while its balances remain
/// in the ledger, through the control totals.
/// Why it matters: the per-policy checks can only examine policies that are present. The
/// control totals are what catch a policy that has vanished altogether.
#[test]
fn reconciliation_notices_balances_with_no_policy_behind_them() {
    let (mut c, id) = book();
    state(&mut c).policies.remove(&id);
    let found = checks(&c);
    assert!(found.contains(&"receivable_control") || found.contains(&"unearned_control"), "{found:?}");
}

/// Purpose: reconciliation reports a suspense account in debit and a ledger whose
/// accounts no longer net to zero.
/// Why it matters: neither can be produced through the posting interface, which is the
/// point: they would mean storage corruption or a bug below the ledger's own checks. The
/// test edits the serialised state directly to prove the alarm is wired up.
#[test]
fn reconciliation_notices_a_ledger_that_was_damaged_underneath_it() {
    let (mut c, id) = book();
    let mut json = serde_json::to_value(&*state(&mut c)).unwrap();
    let suspense = accounts::policy_suspense(&id, NZD).id;
    // The policy holds a 5.00 credit in suspense (stored credit-negative). Flip it to a debit.
    let balance = &mut json["ledger"]["accounts"][suspense.0.as_str()]["balance"];
    assert_eq!(balance.as_i64(), Some(-500));
    *balance = serde_json::json!(700);
    *state(&mut c) = serde_json::from_value(json).unwrap();
    let breaks = reconcile(c.state(0));
    let found: Vec<_> = breaks.iter().map(|b| b.check).collect();
    assert!(found.contains(&"policy_suspense"), "{found:?}");
    let trial = breaks.iter().find(|b| b.check == "trial_balance").expect("the books no longer balance");
    assert!(trial.detail.contains("NZD") && trial.detail.contains("1200"), "{}", trial.detail);
}

/// Purpose: the control totals equal the ledger's role totals with the liability sign
/// turned positive.
/// Why it matters: these five numbers are what the finance ledger's control accounts are
/// checked against each day. A flipped sign would make every daily comparison fail, or
/// worse, pass when it should not.
#[test]
fn control_totals_are_the_role_totals_finance_expects() {
    let (c, id) = book();
    let t = control_totals(c.state(0));
    let p = &c.state(0).policies[&id];
    assert_eq!(t.premium_receivable_minor, p.outstanding_minor() as i128);
    assert_eq!(t.unearned_premium_fine, p.unearned_fine());
    assert!(t.unearned_premium_fine > 0 && t.earned_premium_fine > 0);
    assert_eq!(t.policy_suspense_minor, 500);
    assert_eq!(t.transfer_in_transit, 0);
}

/// Purpose: pins the identifier, unit, class and role of every account in the chart,
/// including the accounts reserved for investment-linked products and policy loans.
/// Why it matters: account identifiers are written into the hash chain and can never
/// change once a ledger exists. This test is the tripwire for an accidental rename, and
/// it documents the chart that later phases must build on.
#[test]
fn the_chart_of_accounts_is_frozen() {
    let p = PolicyId("pol-1".into());
    let fund = FundId("growth".into());
    let minor = AccountUnit::Minor(NZD);
    let fine = AccountUnit::Fine(NZD);
    let units = AccountUnit::Units(fund.clone());
    let chart = [
        (
            accounts::premium_receivable(&p, NZD),
            "pol/pol-1/premium_receivable",
            &minor,
            AccountClass::Asset,
            role::PREMIUM_RECEIVABLE,
        ),
        (
            accounts::unearned_premium(&p, NZD),
            "pol/pol-1/unearned_premium",
            &fine,
            AccountClass::Liability,
            role::UNEARNED_PREMIUM,
        ),
        (
            accounts::policy_suspense(&p, NZD),
            "pol/pol-1/policy_suspense",
            &minor,
            AccountClass::Liability,
            role::POLICY_SUSPENSE,
        ),
        (
            accounts::benefit_payable(&p, NZD),
            "pol/pol-1/benefit_payable",
            &minor,
            AccountClass::Liability,
            role::BENEFIT_PAYABLE,
        ),
        (
            accounts::pending_investment_cash(&p, NZD),
            "pol/pol-1/pending_investment_cash",
            &minor,
            AccountClass::Liability,
            role::PENDING_INVESTMENT_CASH,
        ),
        (
            accounts::unit_account(&p, &fund),
            "pol/pol-1/units/growth",
            &units,
            AccountClass::Liability,
            role::UNIT_ACCOUNT,
        ),
        (
            accounts::policy_loan(&p, NZD),
            "pol/pol-1/policy_loan",
            &minor,
            AccountClass::Asset,
            role::POLICY_LOAN,
        ),
        (accounts::settlement(NZD), "int/settlement/NZD", &minor, AccountClass::Asset, role::SETTLEMENT),
        (
            accounts::earned_premium("term-life", "life", NZD),
            "int/earned_premium/term-life/life/NZD",
            &fine,
            AccountClass::Income,
            role::EARNED_PREMIUM,
        ),
        (
            accounts::gst_payable(NZD),
            "int/gst_payable/NZD",
            &minor,
            AccountClass::Liability,
            role::GST_PAYABLE,
        ),
        (
            accounts::levy_payable("fsl", NZD),
            "int/levy_payable/fsl/NZD",
            &minor,
            AccountClass::Liability,
            role::LEVY_PAYABLE,
        ),
        (
            accounts::withholding_tax_payable("rwt", NZD),
            "int/withholding_tax_payable/rwt/NZD",
            &minor,
            AccountClass::Liability,
            role::WITHHOLDING_TAX_PAYABLE,
        ),
        (
            accounts::lapse_write_off("term-life", NZD),
            "int/lapse_write_off/term-life/NZD",
            &fine,
            AccountClass::Expense,
            role::LAPSE_WRITE_OFF,
        ),
        (accounts::rounding(NZD), "int/rounding/NZD", &fine, AccountClass::Expense, role::ROUNDING),
        (
            accounts::transfer_in_transit(&minor),
            "int/transfer_in_transit/NZD/minor",
            &minor,
            AccountClass::Asset,
            role::TRANSFER_IN_TRANSIT,
        ),
        (
            accounts::transfer_in_transit(&fine),
            "int/transfer_in_transit/NZD/fine",
            &fine,
            AccountClass::Asset,
            role::TRANSFER_IN_TRANSIT,
        ),
        (
            accounts::transfer_in_transit(&units),
            "int/transfer_in_transit/units/growth",
            &units,
            AccountClass::Asset,
            role::TRANSFER_IN_TRANSIT,
        ),
        (
            accounts::claims_offset_in_transit(NZD),
            "int/claims_offset_in_transit/NZD",
            &minor,
            AccountClass::Asset,
            role::CLAIMS_OFFSET_IN_TRANSIT,
        ),
        (
            accounts::fund_unit_position(&fund),
            "int/fund_position/growth/units",
            &units,
            AccountClass::Asset,
            role::FUND_UNIT_POSITION,
        ),
        (
            accounts::fund_cash_position(&fund, NZD),
            "int/fund_position/growth/cash/NZD",
            &minor,
            AccountClass::Liability,
            role::FUND_CASH_POSITION,
        ),
        (
            accounts::reinsurer_account("retail-life-surplus", "reinsurer-north", NZD),
            "int/reinsurer/retail-life-surplus/reinsurer-north/NZD",
            &minor,
            AccountClass::Liability,
            role::REINSURER_ACCOUNT,
        ),
        (
            accounts::reinsurance_premium(NZD),
            "int/reinsurance_premium/NZD",
            &minor,
            AccountClass::Expense,
            role::REINSURANCE_PREMIUM,
        ),
        (
            accounts::reinsurance_commission(NZD),
            "int/reinsurance_commission/NZD",
            &minor,
            AccountClass::Income,
            role::REINSURANCE_COMMISSION,
        ),
        (
            accounts::reinsurance_recoverable(NZD),
            "int/reinsurance_recoverable/NZD",
            &minor,
            AccountClass::Asset,
            role::REINSURANCE_RECOVERABLE,
        ),
        (
            accounts::reinsurance_recoveries(NZD),
            "int/reinsurance_recoveries/NZD",
            &minor,
            AccountClass::Income,
            role::REINSURANCE_RECOVERIES,
        ),
        (
            accounts::reinsurance_profit_commission(NZD),
            "int/reinsurance_profit_commission/NZD",
            &minor,
            AccountClass::Income,
            role::REINSURANCE_PROFIT_COMMISSION,
        ),
        (
            accounts::reinsurance_settlement_clearing(NZD),
            "int/reinsurance_settlement_clearing/NZD",
            &minor,
            AccountClass::Liability,
            role::REINSURANCE_SETTLEMENT_CLEARING,
        ),
        (
            accounts::reinsurance_migration_control(NZD),
            "int/reinsurance_migration_control/NZD",
            &minor,
            AccountClass::Asset,
            role::REINSURANCE_MIGRATION_CONTROL,
        ),
        (
            accounts::migration_clearing("retail-1", NZD),
            "int/migration_clearing/retail-1/NZD",
            &minor,
            AccountClass::Asset,
            role::MIGRATION_CLEARING,
        ),
        (
            accounts::policy_migration_control(NZD),
            "int/policy_migration_control/NZD",
            &minor,
            AccountClass::Asset,
            role::POLICY_MIGRATION_CONTROL,
        ),
    ];
    for (account, id, unit, class, role) in chart {
        assert_eq!(account.id.0, id);
        assert_eq!(&account.unit, unit, "{id}");
        assert_eq!(account.class, class, "{id}");
        assert_eq!(account.role, role, "{id}");
    }
    // The accounts that move with a policy are exactly its own four.
    let moving: Vec<String> = accounts::policy_accounts(&p, NZD).into_iter().map(|a| a.role).collect();
    assert_eq!(
        moving,
        [role::PREMIUM_RECEIVABLE, role::UNEARNED_PREMIUM, role::POLICY_SUSPENSE, role::BENEFIT_PAYABLE]
    );
}

/// Purpose: every command has a unique name, the name is its JSON tag, and at least one
/// module is allowed to send it.
/// Why it matters: the name is what appears in logs, refusals and the idempotency
/// record; the allowed-caller list is the core's own authorisation. A command nobody
/// may send is dead; a command with a mismatched name is unfindable in an incident.
#[test]
fn every_command_has_a_name_an_owner_and_a_matching_wire_tag() {
    let p = PolicyId::from("pol-1");
    let issue = term_life(
        1,
        d(2027, 11, 15),
        d(1990, 3, 10),
        Sex::Female,
        false,
        500_000_00,
        None,
        Frequency::Monthly,
        None,
    );
    let mut all = shipped_activations().unwrap();
    all.extend([
        Command::OpenBusinessDate { date: d(2027, 1, 1) },
        Command::AdvanceBusinessDate { to: d(2027, 1, 2) },
        Command::RunEndOfDay { policies: vec![] },
        Command::IssuePolicy(Box::new(issue.clone())),
        Command::IssueQuoted {
            expected_product_hash: "h".into(),
            issue: Box::new(issue),
        },
        Command::CancelPolicy { policy: p.clone() },
        Command::Reinstate { policy: p.clone() },
        Command::DeclineIndexation {
            policy: p.clone(),
            declined: true,
        },
        Command::MergeLayers {
            policy: p.clone(),
            cover: "life".into(),
        },
        Command::ApplyMembership {
            policy: p.clone(),
            changes: vec![],
        },
        Command::IdentifyMember {
            policy: p.clone(),
            life: LifeId::from("m1"),
            party: PartyId::from("party-1"),
        },
        Command::AddExcessLayer(Box::new(ic_core::command::ExcessLayer {
            continues: None,
            policy: p.clone(),
            cover: "group-life".into(),
            life: LifeId::from("m1"),
            start: d(2027, 1, 1),
            sum_insured_minor: 1,
            loadings: vec![],
            exclusions: vec![],
            loading_itemised: false,
            member_pays: Default::default(),
            reference: "uw".into(),
        })),
        Command::EndExcessLayer {
            policy: p.clone(),
            cover: "group-life".into(),
            layer: 1,
            effective: d(2027, 1, 1),
        },
        Command::AddUnderwrittenLayer(Box::new(ic_core::command::UnderwrittenLayer {
            start: None,
            policy: p.clone(),
            cover: "life".into(),
            life: LifeId("l".into()),
            sum_insured_minor: 1,
            loadings: vec![],
            exclusions: vec![],
            reference: "uw".into(),
        })),
        Command::AlterUnderwrittenTerms(Box::new(ic_core::command::AlteredTerms {
            occupation_class: None,
            policy: p.clone(),
            cover: "life".into(),
            life: LifeId("l".into()),
            loadings: vec![],
            exclusions: vec![],
            reference: "uw".into(),
        })),
        Command::ApproveReinstatement(Box::new(ic_core::command::ReinstatementTerms {
            policy: p.clone(),
            reference: "uw".into(),
            valid_until: d(2027, 1, 1),
            terms: vec![],
        })),
        Command::ReduceCover(Box::new(ic_core::command::ReduceCover {
            policy: p.clone(),
            cover: "life".into(),
            life: LifeId("l".into()),
            sum_insured_minor: 0,
        })),
        Command::AvoidPolicy {
            policy: p.clone(),
            case: "nd".into(),
            premium: ic_core::PremiumOnAvoidance::Refund,
        },
        Command::VaryForNonDisclosure {
            policy: p.clone(),
            case: "nd".into(),
            terms: vec![],
        },
        Command::ApplyMemberReceipt {
            policy: p.clone(),
            life: LifeId("l".into()),
            receipt: "r".into(),
            amount: Money::new(1, NZD),
        },
        Command::RefundMemberCredit {
            policy: p.clone(),
            life: LifeId("l".into()),
            amount: Money::new(1, NZD),
        },
        Command::ReinstateMemberCover {
            policy: p.clone(),
            life: LifeId("l".into()),
            reference: "r".into(),
        },
        Command::RecordRateTableValidation(Box::new(ic_core::command::TableValidation {
            id: "t".into(),
            version: 1,
            content_hash: "h".into(),
            report_hash: "r".into(),
            schema_version: 1,
            by: "x".into(),
        })),
        Command::MakeArrangement {
            policy: p.clone(),
            arrangement: "a".into(),
            instalments: vec![],
        },
        Command::EndArrangement {
            policy: p.clone(),
            arrangement: "a".into(),
        },
        Command::GrantHardship {
            policy: p.clone(),
            hardship: "h".into(),
            from: d(2027, 1, 1),
            to: d(2027, 2, 1),
            share_bps: 0,
        },
        Command::EndHardship {
            policy: p.clone(),
            hardship: "h".into(),
        },
        Command::ApplyReceipt {
            policy: p.clone(),
            receipt: "r".into(),
            amount: Money::new(1, NZD),
        },
        Command::ReverseReceipt {
            policy: p.clone(),
            receipt: "r".into(),
        },
        Command::RefundSuspense {
            policy: p.clone(),
            amount: Money::new(1, NZD),
            with_claim: None,
        },
        Command::RaiseRefundPayable {
            policy: p.clone(),
            refund: "rf".into(),
            amount: Money::new(1, NZD),
            with_claim: None,
        },
        Command::ReleaseRefundPayable {
            policy: p.clone(),
            refund: "rf".into(),
        },
        Command::SettlePayable {
            policy: p.clone(),
            payment: "pay".into(),
            payable: ic_core::PayableKind::Refund,
            refund: Some("rf".into()),
            amount: Money::new(1, NZD),
            bank_account: "nz-main-refunds".into(),
        },
        Command::ReturnPayment {
            policy: p.clone(),
            payment: "pay".into(),
        },
        Command::RecordCession {
            policy: p.clone(),
            cover: "life".into(),
            layer: 1,
            life: LifeId::from("life-1"),
            from: d(2027, 1, 1),
            segments: vec![ic_core::reinsurance::CessionSegment {
                from: d(2027, 1, 1),
                cession: None,
            }],
        },
        Command::PostReinsurance {
            kind: ic_core::reinsurance::ReinsurancePosting::CededPremium,
            treaty: "t".into(),
            reinsurer: "r".into(),
            currency: NZD,
            policy: Some(p.clone()),
            amount_minor: 1,
            value_date: d(2027, 1, 1),
            reference: "ref".into(),
            bank_account: None,
        },
        Command::BeginTransfer {
            policy: p.clone(),
            transfer_id: "t".into(),
            target_group: GroupId::from("g"),
            target_partition: 1,
        },
        Command::CompleteTransfer {
            policy: p.clone(),
            transfer_id: "t".into(),
        },
        Command::AbortTransfer {
            policy: p.clone(),
            transfer_id: "t".into(),
        },
        Command::Claims(Box::new(ic_core::claims_module::Command::DrainDue)),
        Command::PostCounterparty(Box::new(ic_core::counterparty::CounterpartyPosting {
            reference: "ce:0-1".into(),
            kind: "commission_earned".into(),
            currency: NZD,
            value_date: None,
            policy: Some(p.clone()),
            reverses: None,
            lines: vec![ic_core::counterparty::CounterpartyLine {
                account: ic_core::counterparty::CounterpartyAccount::Party {
                    party: PartyId("payee-1".into()),
                    ledger: ic_core::counterparty::PartyLedger::CommissionPayable,
                },
                amount_minor: -1,
            }],
            detail: Default::default(),
        })),
        Command::Investment(Box::new(ic_core::invest::InvestCommand::CancelOrder { order_id: "o-1".into() })),
        Command::Investment(Box::new(ic_core::invest::InvestCommand::DirectPayout {
            policy: p.clone(),
            payout: "wd-1".into(),
            payee: ic_core::PartyId("party-oa".into()),
            payment: "wd-1".into(),
            direction: "vault/dir-1".into(),
        })),
        Command::Interest(Box::new(ic_core::interest::InterestCommand::Release {
            policy: p.clone(),
            interest: "int-1".into(),
            effective: d(2027, 11, 15),
            discharge: "vault/d-1".into(),
        })),
        Command::ChangeExcess(Box::new(ic_core::command::ChangedExcess {
            policy: p.clone(),
            cover: "hospital".into(),
            life: LifeId("life-1".into()),
            excess_option: "high".into(),
        })),
        Command::AddCover(Box::new(ic_core::command::AddedCover {
            excess_option: None,
            policy: p.clone(),
            cover: "dental".into(),
            life: LifeId("life-1".into()),
            sum_insured_minor: 0,
            loadings: vec![],
            exclusions: vec![],
            reference: "req-1".into(),
        })),
        Command::SetOwnerHold(Box::new(ic_core::owner_hold::SetOwnerHold {
            policy: p.clone(),
            held: true,
            reference: "hold-1".into(),
        })),
        Command::SetDiscountOverride(Box::new(ic_core::discount_override::SetDiscountOverride {
            policy: p.clone(),
            terms: ic_core::discount_override::DiscountOverride {
                id: "ovr-1".into(),
                percent: ic_money::DecStr::parse("10").unwrap(),
                covers: vec!["life".into()],
                life: None,
                reason: "retention".into(),
                from: d(2027, 11, 15),
                until: d(2028, 11, 15),
                approver: "staff-1".into(),
                second_approver: Some("staff-2".into()),
            },
        })),
        Command::SplitScheme(Box::new(ic_core::SplitScheme {
            policy: p.clone(),
            new_policy: PolicyId("pol-2".into()),
            new_number: "N2".into(),
            lives: vec![LifeId("life-1".into())],
            effective: d(2027, 11, 15),
        })),
        Command::SetMemberCovers(Box::new(ic_core::SetMemberCovers {
            policy: p.clone(),
            life: LifeId("life-1".into()),
            covers: vec![],
            effective: d(2027, 11, 15),
            reference: "enrol-1".into(),
        })),
        Command::PayeeDebt(Box::new(ic_core::payee_debt::PayeeDebtCommand::WriteOff {
            policy: p.clone(),
            debt: "debt-1".into(),
            amount: Money::new(1, NZD),
            reason: "untraceable".into(),
        })),
        Command::MakePaidUp(Box::new(ic_core::paid_up::MakePaidUp {
            policy: p.clone(),
            cover: "life".into(),
            life: LifeId("life-1".into()),
            reference: "pu-1".into(),
        })),
        Command::SuspendCover(Box::new(ic_core::suspension::SuspendCover {
            policy: p.clone(),
            reference: "sus-1".into(),
            from: NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(),
            to: NaiveDate::from_ymd_opt(2028, 4, 1).unwrap(),
        })),
        Command::WithdrawConfig(Box::new(ic_core::catalog::Withdrawal {
            kind: ic_core::catalog::ConfigKind::RateTable,
            id: "life-base".into(),
            version: 1,
            on: None,
            reason: "wrong rates".into(),
            change: "CHG-1".into(),
            approved_by: vec!["ops-ann".into()],
            retire: false,
        })),
        Command::Migration(Box::new(ic_core::migration::MigrationCommand::Release {
            cohort: "retail-1".into(),
            limit: 10,
        })),
        Command::ChangeBilling(Box::new(ic_core::billing_change::ChangeBilling {
            policy: p.clone(),
            reference: "bill-1".into(),
            frequency: Some(ic_product::Frequency::Yearly),
            billing_day: None,
        })),
        Command::ResumeCover {
            policy: p.clone(),
            reference: "sus-1".into(),
        },
        Command::RemitWithholding(Box::new(ic_core::remittance::Remittance {
            kind: "rwt".into(),
            period: "2027-11".into(),
            payment: "ir-1".into(),
            amount: Money::new(1, NZD),
            bank_account: "main".into(),
            statutory_fund: None,
        })),
        Command::ReturnRemittance {
            kind: "rwt".into(),
            payment: "ir-1".into(),
        },
        Command::RecordReplacement(Box::new(ic_core::command::Replacement {
            policy: p.clone(),
            replaced_policy: PolicyId("pol-old".into()),
            life: LifeId("life-1".into()),
            replaced_life: LifeId("life-1".into()),
            cover: "life".into(),
            replaced_cover: "life".into(),
            replaced_sum_insured_minor: 1,
            evidence: "vault/r-1".into(),
        })),
        Command::ExerciseOption(Box::new(ic_core::command::OptionExercise {
            policy: p.clone(),
            cover: "life".into(),
            life: LifeId("life-1".into()),
            event: "marriage".into(),
            event_date: d(2027, 11, 1),
            sum_insured_minor: 1,
            evidence: "vault/e-1".into(),
            reference: "opt-1".into(),
            from_cover: None,
            excess_option: None,
        })),
        Command::Loan(Box::new(ic_core::loan::LoanCommand::Draw {
            policy: p.clone(),
            reference: "loan-1".into(),
            amount_minor: 1,
        })),
        Command::SetEntitlement(Box::new(ic_core::entitlement::SetEntitlement {
            policy: p.clone(),
            code: "wellness".into(),
            tier: Some("2".into()),
            life: Some(LifeId("life-1".into())),
            held: true,
            from: d(2027, 11, 15),
            until: None,
            reference: "award-1".into(),
        })),
        Command::Nominate(Box::new(ic_core::nomination::Nominate {
            policy: p.clone(),
            nomination: "nom-1".into(),
            life: None,
            nominees: vec![],
            effective_from: d(2027, 11, 15),
            form: "vault/f-1".into(),
            conversation: None,
        })),
    ]);
    let mut names = std::collections::BTreeSet::new();
    for command in &all {
        let name = command.name();
        names.insert(name);
        assert!(!command.allowed_callers().is_empty(), "{name} has no owner");
        let json = serde_json::to_value(command).unwrap();
        assert!(json.get(name).is_some(), "{name} is not the wire tag of {json}");
        // Compared as JSON: a definition's wording is deliberately not carried, only its hash.
        let back = serde_json::from_value::<Command>(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), json, "{name} does not survive the log's encoding");
    }
    // land_transfer needs a real record and is exercised by the transfer tests.
    assert_eq!(names.len(), 67, "{names:?}");
}

/// Purpose: a quote returns exactly the amounts the policy is then billed, for a retail
/// policy and for a group scheme priced per member.
/// Why it matters: quote and issue share one pricing path by design. If they ever
/// diverged, customers would be billed something other than what they were quoted.
#[test]
fn a_quote_equals_the_first_bill() {
    let (mut c, _) = book();
    let today = c.business_date().unwrap();
    let retail = term_life(
        2,
        today,
        d(1985, 7, 1),
        Sex::Male,
        true,
        750_000_00,
        Some(150_000_00),
        Frequency::Fortnightly,
        Some("0.25"),
    );
    let scheme = group_scheme(
        3,
        today,
        (1..=5).map(|i| member(&format!("m{i}"), d(1970 + i, 1, 1), "executive", 150_000_00)).collect(),
    );
    for application in [retail, scheme] {
        let quoted = quote(&c.state(0).catalog, application.clone(), today).unwrap();
        let id = application.policy.clone();
        c.submit(0, "new-business", issue_command(application)).unwrap();
        assert_eq!(c.state(0).policies[&id].outstanding_minor(), quoted.total());
        assert!(quoted.total() > 0);
    }
    // A quote for something that cannot be issued is refused the same way issue would be.
    let mut bad = term_life(4, today, d(1985, 7, 1), Sex::Male, true, 750_000_00, None, Frequency::Monthly, None);
    bad.product_code = "nope".into();
    assert_eq!(quote(&c.state(0).catalog, bad, today).unwrap_err().code, "unknown_product");
}

/// Purpose: on a level premium product the original cover keeps the rate for the age at
/// which it started, where a stepped product would move to the next age band.
/// Why it matters: level premium is a promise that the price will not rise with age. No
/// shipped product uses it yet, so without this test the promise would be untested until
/// the first level product reached its first anniversary in production.
#[test]
fn a_level_premium_does_not_rise_with_age() {
    let (mut c, _) = book();
    let source = include_str!("../../../products/term_life.star")
        .replace("code = \"term-life\"", "code = \"term-life-level\"")
        .replace("rating = stepped(\"life-base\")", "rating = level(\"life-base\", 65)");
    c.activate_everywhere(Command::ActivateProduct {
        product: Box::new(load_product("level.star", &source).unwrap()),
    })
    .unwrap();
    let today = c.business_date().unwrap();
    // Aged 37 at issue; 38 at the first anniversary is the next band of the rate table.
    let mut issue = term_life(5, today, d(1990, 3, 10), Sex::Female, false, 500_000_00, None, Frequency::Monthly, None);
    issue.product_code = "term-life-level".into();
    let id = issue.policy.clone();
    c.submit(0, "new-business", issue_command(issue)).unwrap();
    c.submit(
        0,
        "policy-servicing",
        Command::DeclineIndexation {
            policy: id.clone(),
            declined: true,
        },
    )
    .unwrap();
    c.submit(
        0,
        "cash-matching",
        Command::ApplyReceipt {
            policy: id.clone(),
            receipt: "ahead".into(),
            amount: Money::new(14 * MONTHLY, NZD),
        },
    )
    .unwrap();
    for _ in 0..367 {
        assert!(c.end_of_day().unwrap().breaks.is_empty());
    }
    let p = &c.state(0).policies[&id];
    assert_eq!(p.anniversaries_processed, 1);
    let bills: Vec<i64> = p.receivables.iter().map(|r| r.total_minor).collect();
    assert_eq!(bills.len(), 13);
    assert!(bills.iter().all(|b| *b == MONTHLY), "{bills:?}");
}

/// Purpose: the replicated state hash changes when policy state or configuration changes,
/// even where the ledger is untouched, and the whole state survives a snapshot round trip.
/// Why it matters: replicas compare this hash to detect divergence. Writing this test
/// exposed that the hash had silently covered the ledger only, because the catalog could
/// not be encoded; two replicas disagreeing about a policy's terms would have matched.
#[test]
fn the_state_hash_covers_policies_and_configuration_not_just_the_ledger() {
    let (mut c, id) = book();
    let before = c.state(0).state_hash();
    let ledger_before = c.state(0).ledger.digest();

    let snapshot = serde_json::to_string(c.state(0)).expect("the whole state encodes");
    let restored: CoreState = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(restored.state_hash(), before, "a restored snapshot is the same state");
    assert_eq!(&restored, c.state(0));

    state(&mut c).policies.get_mut(&id).unwrap().indexation_declined = true;
    assert_eq!(c.state(0).ledger.digest(), ledger_before, "the ledger did not change");
    let policy_changed = c.state(0).state_hash();
    assert_ne!(policy_changed, before, "but the state did");

    let rate = ic_money::DecStr::parse("0.031").unwrap();
    c.submit(
        0,
        "config",
        Command::ActivateIndexRate {
            code: "cpi".into(),
            effective_from: d(2028, 1, 1),
            rate,
        },
    )
    .unwrap();
    assert_ne!(c.state(0).state_hash(), policy_changed);
}
