//! Paying an account abroad (CLM-028): a payee with a foreign account is
//! checked like any other, and paid through the route's cross-border
//! channel, the bank converting from the run's currency.

mod common;

use common::*;
use ic_payments::config::WireFormat;
use ic_payments::iso20022::{read_initiation, OrderingAccount};
use ic_payments::model::{ForeignAccount, ItemStatus, NameCheck, PayeeKind, RunType};
use ic_payments::Command;

fn lagos() -> ForeignAccount {
    ForeignAccount {
        bic: "ZEIBNGLA".to_string(),
        country: "NG".to_string(),
        currency: "NGN".to_string(),
        address: "12 Broad Street, Lagos".to_string(),
    }
}

fn register(payee: &str, number: &str, foreign: ForeignAccount) -> Command {
    Command::RegisterForeignPayee {
        payee: payee.to_string(),
        party: format!("party-{payee}"),
        kind: PayeeKind::Beneficiary,
        account: account("Adaeze Okafor", number),
        foreign,
        name_check: NameCheck::Unavailable,
    }
}

fn with_cross_border(desk: &mut Desk, channel: &str, run_type: RunType) -> Result<(), String> {
    let mut config = config();
    config.routes.get_mut(&run_type).unwrap().cross_border = Some(channel.to_string());
    let version = desk.state.config_version + 1;
    desk.send(
        &cal(),
        Command::ActivateConfig {
            version,
            config: Box::new(config),
        },
    )
    .map(|_| ())
    .map_err(|r| r.message)
}

/// Purpose: a foreign account is refused unless it has a number of
/// capitals and digits, a BIC of 8 or 11, a country other than New Zealand,
/// a currency and an address; a change to its number is held to the same
/// shape; and a cross-border channel must pay out, at the primary channel's
/// bank.
/// Why it matters: a payment abroad that the bank cannot route comes back
/// days later less the charges, and a beneficiary waits on a claim.
#[test]
fn an_account_abroad_is_checked_before_it_is_taken() {
    let mut desk = Desk::new();
    let bad = |f: fn(&mut ForeignAccount)| {
        let mut a = lagos();
        f(&mut a);
        a
    };
    for wrong in [
        bad(|a| a.bic = "ZEIBNGL".into()),
        bad(|a| a.country = "NZ".into()),
        bad(|a| a.currency = "ngn".into()),
        bad(|a| a.address = " ".into()),
    ] {
        assert_eq!(desk.code(&pam(), register("f1", "0123456789", wrong)), "invalid_account");
    }
    assert_eq!(desk.code(&pam(), register("f1", "01-23", lagos())), "invalid_account");
    desk.ok(&pam(), register("f1", "0123456789", lagos()));
    assert_eq!(desk.state.payees["f1"].foreign, Some(lagos()));
    let change = |number: &str| Command::ChangePayeeAccount {
        payee: "f1".to_string(),
        account: account("Adaeze Okafor", number),
        name_check: NameCheck::Unavailable,
    };
    assert_eq!(desk.code(&pam(), change("abc12345")), "invalid_account");
    desk.ok(&pam(), change("9876543210"));

    assert!(with_cross_border(&mut desk, "bnz-file", RunType::LumpSumClaims)
        .unwrap_err()
        .contains("cross-border"));
    assert!(with_cross_border(&mut desk, "anz-api", RunType::Collection)
        .unwrap_err()
        .contains("cross-border"));
    assert!(with_cross_border(&mut desk, "nowhere", RunType::LumpSumClaims)
        .unwrap_err()
        .contains("unknown channel"));
}

/// Purpose: a payment to a foreign payee is held where the route has no
/// cross-border channel; once it has one, the run sends home payments to
/// the route's channel and the payment abroad in its own submission to the
/// cross-border channel, under the same item ID; the file carries the
/// beneficiary's bank, country, currency and address, and the ISO 20022
/// message asks the bank to convert the run's amount, names the bank by
/// BIC and writes an IBAN as one.
/// Why it matters: the item ID is the idempotency key at the bank whichever
/// rail it goes by, and a payment abroad sent down the domestic channel
/// would be refused or, worse, paid to a New Zealand account of the same
/// number.
#[test]
fn a_payment_abroad_goes_over_the_cross_border_channel() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0201111111");
    desk.ok(
        &pam(),
        register(
            "f1",
            "GB33BUKB20201555555555",
            ForeignAccount {
                country: "GB".into(),
                currency: "GBP".into(),
                ..lagos()
            },
        ),
    );
    desk.ok(&pat(), Command::ConfirmPayee { payee: "f1".to_string() });
    desk.ok(
        &svc(),
        Command::ScreenPayee {
            payee: "f1".to_string(),
            clear: true,
        },
    );
    desk.ok(&pam(), register("f2", "0123456789", lagos()));
    desk.ok(&pat(), Command::ConfirmPayee { payee: "f2".to_string() });
    desk.ok(
        &svc(),
        Command::ScreenPayee {
            payee: "f2".to_string(),
            clear: true,
        },
    );

    desk.prepare("r0", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r0", vec![pay("i0", "f1", 100_00)]);
    desk.ok(&mia(), Command::CheckRun { run: "r0".to_string() });
    assert_eq!(
        desk.state.runs["r0"].items["i0"].status,
        ItemStatus::Held("no_cross_border_channel".to_string())
    );
    desk.ok(&mia(), Command::CancelRun { run: "r0".to_string() });

    with_cross_border(&mut desk, "anz-api", RunType::LumpSumClaims).unwrap();
    desk.prepare("r1", RunType::LumpSumClaims, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 200_00), pay("i2", "f1", 300_00), pay("i3", "f2", 400_00)]);
    let first = desk.approve("r1");
    let submissions = desk.state.runs["r1"].submissions.clone();
    assert_eq!(submissions, ["r1-1", "r1-2"]);
    assert_eq!(first, "r1-2");
    assert_eq!(desk.state.submissions["r1-1"].channel, "anz-file");
    assert_eq!(desk.state.submissions["r1-1"].items, ["i1"]);
    assert_eq!(desk.state.submissions["r1-2"].channel, "anz-api");
    assert_eq!(desk.state.submissions["r1-2"].items, ["i2", "i3"]);

    let mut batch = desk.state.batch("r1-2").unwrap();
    assert!(batch.file().contains("F|i3|ZEIBNGLA|NG|NGN|12 Broad Street, Lagos"));
    batch.format = WireFormat::Iso20022;
    batch.ordering = Some(OrderingAccount {
        name: "Insurer".to_string(),
        number: "0100001".to_string(),
        bic: None,
    });
    let xml = batch.wire().unwrap();
    assert!(xml.contains("<EqvtAmt><Amt Ccy=\"NZD\">300.00</Amt><CcyOfTrf>GBP</CcyOfTrf></EqvtAmt>"));
    assert!(xml.contains("<IBAN>GB33BUKB20201555555555</IBAN>") && xml.contains("<Othr><Id>0123456789</Id></Othr>"));
    assert!(xml.contains("<BICFI>ZEIBNGLA</BICFI>") && xml.contains("<Ctry>NG</Ctry>") && xml.contains("<ChrgBr>SHAR</ChrgBr>"));
    let read = read_initiation(&xml).unwrap();
    assert_eq!(read.items, vec![("i2".to_string(), 300_00), ("i3".to_string(), 400_00)]);
    batch.items[0].foreign.as_mut().unwrap().bic = "BAD BIC".to_string();
    assert!(batch.wire().unwrap_err().contains("BIC"));
}
