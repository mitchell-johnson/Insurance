//! Paying withheld tax to Inland Revenue (`tax.rs`).

mod common;

use common::*;
use ic_payments::model::{Instruction, PayeeKind, ReturnReason, RunType};
use ic_payments::query::answer;
use ic_payments::tax::{Share, TaxStatus};
use ic_payments::Command;
use serde_json::json;

fn request(id: &str, kind: &str, period: &str) -> Command {
    Command::RequestTaxRemittance {
        remittance: id.to_string(),
        kind: kind.to_string(),
        period: period.to_string(),
        payee: "ird".to_string(),
    }
}

fn share(partition: u16, fund: Option<&str>, amount_minor: i64) -> Share {
    Share {
        partition,
        fund: fund.map(str::to_string),
        amount_minor,
    }
}

fn price(id: &str, due: i64, shares: Vec<Share>) -> Command {
    Command::PriceTaxRemittance {
        remittance: id.to_string(),
        due_minor: due,
        due_on: d(2027, 12, 20),
        shares,
    }
}

fn work(desk: &Desk) -> Vec<(String, Instruction)> {
    desk.state.outbox.values().map(|e| (e.key.clone(), e.instruction.clone())).collect()
}

fn drain(desk: &mut Desk) {
    while let Some(&entry) = desk.state.outbox.keys().next() {
        desk.ok(&svc(), Command::CompleteInstruction { entry, refused: None });
    }
}

/// Purpose: a maker asks for a month's withheld tax to be paid; the shares
/// the partitions hold must add up to finance's figure, or treasury is told
/// once and nothing is paid; once they do, the payment waits for a tax run,
/// and when it settles each partition and fund is told its share under its
/// own payment ID, against the account it was paid from; a return after
/// settlement tells each partition it owes its share again.
/// Why it matters: the revenue is paid exactly what the books closed on, and
/// the core's liability falls only when money has actually left the bank.
#[test]
fn withheld_tax_is_paid_to_the_revenue_and_discharged_on_each_partition() {
    let mut desk = Desk::new();
    desk.payee("ird", PayeeKind::Authority, "0390000000");
    assert_eq!(desk.code(&rae(), request("tax-1", "rwt", "2027-11")), "not_permitted");
    assert_eq!(desk.code(&mia(), request("tax-1", "RWT", "2027-11")), "invalid_remittance");
    assert_eq!(desk.code(&mia(), request("tax-1", "rwt", "2027-13")), "invalid_remittance");
    assert_eq!(desk.code(&mia(), request("tax 1", "rwt", "2027-11")), "invalid_remittance");
    assert_eq!(
        desk.code(
            &mia(),
            Command::RequestTaxRemittance {
                remittance: "tax-1".into(),
                kind: "rwt".into(),
                period: "2027-11".into(),
                payee: "nobody".into()
            }
        ),
        "unknown_payee"
    );
    desk.ok(&mia(), request("tax-1", "rwt", "2027-11"));
    assert_eq!(desk.code(&mia(), request("tax-1", "benefit", "2027-11")), "duplicate_remittance");
    assert_eq!(desk.code(&mia(), request("tax-2", "rwt", "2027-11")), "duplicate_remittance");
    let to_price = answer(&desk.state, json!({"q": "tax_remittances_to_price"})).unwrap();
    assert_eq!(to_price[0][0], "tax-1");

    assert_eq!(desk.code(&mia(), price("tax-1", 300_00, vec![])), "not_permitted");
    desk.ok(
        &svc(),
        price("tax-1", 300_00, vec![share(0, Some("sf1"), 100_00), share(1, Some("sf1"), 150_00)]),
    );
    let told = work(&desk);
    assert!(matches!(&told[0].1, Instruction::Notify { subject, detail, .. } if subject == "tax_shares_do_not_add_up" && detail.contains("hold 25000")));
    desk.ok(
        &svc(),
        price("tax-1", 300_00, vec![share(0, Some("sf1"), 100_00), share(1, Some("sf1"), 150_00)]),
    );
    assert_eq!(work(&desk).len(), 1, "told once for the same figures");
    drain(&mut desk);
    assert!(desk.state.waiting.is_empty());

    let shares = vec![
        share(0, Some("sf1"), 100_00),
        share(1, Some("sf1"), 150_00),
        share(1, Some("sf2"), 50_00),
        share(2, None, 0),
    ];
    desk.ok(&svc(), price("tax-1", 300_00, shares));
    assert_eq!(desk.code(&svc(), price("tax-1", 300_00, vec![share(0, None, 300_00)])), "unknown_remittance");
    let status = &desk.state.tax_remittances["tax-1"];
    assert_eq!((status.status.clone(), status.shares.len()), (TaxStatus::Priced, 3));

    desk.prepare("t1", RunType::TaxAndLevies, d(2027, 11, 15));
    desk.ok(
        &mia(),
        Command::TakeWaiting {
            run: "t1".to_string(),
            payments: ["tax-1".to_string()].into(),
            payees: Default::default(),
        },
    );
    let s = desk.approve("t1");
    desk.settle(&s, d(2027, 11, 15));
    let remitted: Vec<_> = work(&desk)
        .into_iter()
        .filter_map(|(key, i)| match i {
            Instruction::CorePartition { partition, caller, command } => Some((key, partition, caller, serde_json::to_value(command).unwrap())),
            _ => None,
        })
        .collect();
    assert_eq!(remitted.len(), 3);
    assert_eq!(
        (remitted[0].0.as_str(), remitted[0].1, remitted[0].2.as_str()),
        ("tax-remitted-tax-1:1", 0, "payments")
    );
    let third = &remitted[2].3["remit_withholding"];
    assert_eq!(
        (third["payment"].clone(), third["statutory_fund"].clone(), third["amount"]["minor"].clone()),
        (json!("tax-1:3"), json!("sf2"), json!(50_00))
    );
    assert_eq!(third["bank_account"], "anz-sf1-third");
    assert_eq!(desk.state.tax_remittances["tax-1"].status, TaxStatus::Paid);
    drain(&mut desk);

    desk.ok(
        &svc(),
        Command::RecordReturn {
            item: "tax-1".to_string(),
            reason: ReturnReason::InvalidAccount,
            returned_on: d(2027, 11, 16),
        },
    );
    let back: Vec<_> = work(&desk).into_iter().filter(|(k, _)| k.starts_with("tax-returned-")).collect();
    assert_eq!(back.len(), 3);
    let Instruction::CorePartition { command, .. } = &back[1].1 else {
        panic!("to a partition")
    };
    assert_eq!(
        serde_json::to_value(command).unwrap(),
        json!({"return_remittance": {"kind": "rwt", "payment": "tax-1:2"}})
    );
    assert_eq!(desk.state.tax_remittances["tax-1"].status, TaxStatus::Returned);
    desk.ok(&mia(), request("tax-2", "rwt", "2027-11"));
}
