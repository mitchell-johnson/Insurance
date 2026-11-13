//! The batch a submission becomes, and the messages a bank sends back.

mod common;

use common::*;
use ic_payments::file::{BankMessage, Direction};
use ic_payments::model::{AccountDetails, NameCheck, PayeeKind, ReturnReason, RunType, StatementLine};
use ic_payments::Command;

/// Purpose: a submission becomes a file with a header, a line per item and
/// a trailer carrying the count and total; the text and its hash depend only
/// on the batch; a separator or a line break inside a name cannot break the
/// file's structure.
/// Why it matters: the bank checks the trailer before it pays anything, and
/// the hash is how a repeat of the same file is told from a different file
/// with the same ID. A payee named "A|9999" must not become an extra field.
#[test]
fn a_submission_becomes_a_file_the_bank_can_check() {
    let mut desk = Desk::new();
    desk.payee("p1", PayeeKind::Customer, "0101234567");
    desk.ok(
        &pam(),
        Command::RegisterPayee {
            payee: "p2".to_string(),
            party: "x".to_string(),
            kind: PayeeKind::Provider,
            account: AccountDetails {
                name: "Dr A|B\nClinic".to_string(),
                number: "02-0765-4321".to_string(),
            },
            name_check: NameCheck::Match,
        },
    );
    desk.ok(&pat(), Command::ConfirmPayee { payee: "p2".to_string() });
    desk.ok(
        &svc(),
        Command::ScreenPayee {
            payee: "p2".to_string(),
            clear: true,
        },
    );
    desk.prepare("r1", RunType::ProviderPayments, d(2027, 11, 15));
    desk.add("r1", vec![pay("i1", "p1", 300_00), pay("i2", "p2", 45_50)]);
    assert!(desk.state.batch("r1-1").is_none());
    let s = desk.approve("r1");
    let batch = desk.state.batch(&s).unwrap();
    assert_eq!(batch.direction, Direction::Credit);
    assert_eq!(
        batch.file(),
        "H|ICPAY1|r1-1|anz-sf1-claims|CREDIT|2027-11-15|NZD\nD|i1|p1|0101234567|30000\nD|i2|Dr A B Clinic|02-0765-4321|4550\nT|2|34550\n"
    );
    assert_eq!(batch.wire().unwrap(), batch.file(), "a channel in the system's own format is sent the file");
    assert_eq!(batch.content_hash().unwrap().len(), 64);
    assert_eq!(batch.content_hash(), desk.state.batch(&s).unwrap().content_hash());
    let manifest = batch.manifest("RS256".to_string(), "key-1".to_string(), "sig".to_string()).unwrap();
    assert_eq!((manifest.items, manifest.total_minor, manifest.bytes), (2, 345_50, batch.file().len()));
    assert_eq!(manifest.sha256, batch.content_hash().unwrap());

    desk.authority("a1", "pol-1", "0309999999", false);
    desk.prepare("dd", RunType::Collection, d(2027, 11, 15));
    desk.add("dd", vec![collect("c1", "a1", "pol-1", 10_00)]);
    let s = desk.approve("dd");
    assert!(desk.state.batch(&s).unwrap().file().starts_with("H|ICPAY1|dd-1|anz-sf1-collection|DEBIT|"));
}

/// Purpose: each kind of message from a bank becomes the one module command
/// that records it, with nothing lost on the way, and survives its JSON form.
/// Why it matters: this mapping is the whole inbound side of a channel. A
/// field dropped here is a rejection or a return the module never hears of.
#[test]
fn a_bank_message_becomes_the_command_that_records_it() {
    let messages = vec![
        BankMessage::Acknowledgement {
            id: "r1-1".to_string(),
            refused: None,
            rejected: [("i2".to_string(), "invalid".to_string())].into(),
        },
        BankMessage::Settlement {
            id: "r1-1".to_string(),
            settled_on: d(2027, 11, 15),
        },
        BankMessage::Return {
            item: "i1".to_string(),
            reason: ReturnReason::AccountClosed,
            returned_on: d(2027, 11, 16),
        },
        BankMessage::Statement {
            account: "anz-sf1-claims".to_string(),
            date: d(2027, 11, 16),
            opening_minor: 10,
            closing_minor: 15,
            lines: vec![StatementLine {
                id: "l1".to_string(),
                amount_minor: 5,
                reference: "x".to_string(),
                payer_account: None,
            }],
        },
    ];
    let text = serde_json::to_string(&messages).unwrap();
    let back: Vec<BankMessage> = serde_json::from_str(&text).unwrap();
    assert_eq!(back, messages);
    let commands: Vec<Command> = back.into_iter().map(BankMessage::command).collect();
    assert!(matches!(&commands[0], Command::RecordAcknowledgement { submission, refused: None, rejected } if submission == "r1-1" && rejected.len() == 1));
    assert!(matches!(&commands[1], Command::RecordSettlement { submission, settled_on } if submission == "r1-1" && *settled_on == d(2027, 11, 15)));
    assert!(matches!(&commands[2], Command::RecordReturn { item, reason: ReturnReason::AccountClosed, .. } if item == "i1"));
    assert!(
        matches!(&commands[3], Command::RecordStatement { account, opening_minor: 10, closing_minor: 15, lines, .. } if account == "anz-sf1-claims" && lines.len() == 1)
    );
    let short: BankMessage = serde_json::from_str(r#"{"type":"acknowledgement","id":"r1-1"}"#).unwrap();
    assert!(matches!(short.command(), Command::RecordAcknowledgement { rejected, .. } if rejected.is_empty()));
}

/// Purpose: a channel configured for ISO 20022 is sent its submissions as a
/// `pain.001` naming the insurer's account as that bank knows it, hashed and
/// counted over the XML; a bank spoken to in ISO 20022 cannot be configured
/// without those account details, or with a region that presents it no
/// client certificate; and a run or item ID too long to travel
/// as a bank reference is refused when it is made, not when it is sent.
/// Why it matters: the hash the module records must be the hash of the
/// bytes the bank holds, or a repeat could not be told from a new file. An
/// ID found too long at sending time would leave approved money stuck with
/// nobody told why.
#[test]
fn a_bank_spoken_to_in_iso_20022_is_sent_iso_20022() {
    let mut desk = Desk::bare();
    let mut missing = iso_config();
    missing.accounts.get_mut("anz-sf1-claims").unwrap().at_bank = None;
    let activate = |version, config| Command::ActivateConfig {
        version,
        config: Box::new(config),
    };
    assert_eq!(desk.code(&cal(), activate(1, missing)), "invalid_config");
    let mut blank = iso_config();
    blank.accounts.get_mut("anz-sf1-claims").unwrap().at_bank.as_mut().unwrap().number = " ".to_string();
    assert_eq!(desk.code(&cal(), activate(1, blank)), "invalid_config");
    let mut uncertified = iso_config();
    uncertified
        .channels
        .get_mut("anz-file")
        .unwrap()
        .endpoints
        .get_mut("akl")
        .unwrap()
        .client_certificate = None;
    assert_eq!(desk.code(&cal(), activate(1, uncertified)), "region_not_provisioned");
    let mut other_bank = iso_config();
    other_bank.accounts.get_mut("bnz-sf1-claims").unwrap().at_bank = None;
    desk.ok(&cal(), activate(1, other_bank));
    desk.statement("anz-sf1-claims", d(2027, 11, 12), 1_000_000_00, &[]);

    desk.payee("p1", PayeeKind::Customer, "0101234567");
    let prepare = |run: &str| Command::PrepareRun {
        run: run.to_string(),
        run_type: RunType::ProviderPayments,
        fund: "sf1".to_string(),
        currency: ic_money::Currency::NZD,
        value_date: d(2027, 11, 15),
        urgent: false,
    };
    assert_eq!(desk.code(&mia(), prepare(&"r".repeat(32))), "invalid_run");
    assert_eq!(desk.code(&mia(), prepare("run 1")), "invalid_run");
    desk.ok(&mia(), prepare(&"r".repeat(31)));
    desk.prepare("r1", RunType::ProviderPayments, d(2027, 11, 15));
    let long = Command::AddRunItems {
        run: "r1".to_string(),
        items: vec![pay(&"i".repeat(36), "p1", 1_00)],
    };
    assert_eq!(desk.code(&mia(), long), "invalid_item");
    desk.add("r1", vec![pay("i1", "p1", 300_00)]);
    let s = desk.approve("r1");
    let batch = desk.state.batch(&s).unwrap();
    assert_eq!(batch.format, ic_payments::config::WireFormat::Iso20022);
    let xml = batch.wire().unwrap();
    assert!(xml.contains("pain.001.001.09"));
    assert!(xml.contains("<Dbtr><Nm>Example Life Ltd</Nm></Dbtr><DbtrAcct><Id><Othr><Id>01-0100-0000014-000</Id>"));
    assert!(xml.contains("<BICFI>ANZBNZ22</BICFI>"));
    let manifest = batch.manifest("RS256".to_string(), "k".to_string(), "sig".to_string()).unwrap();
    assert_eq!(manifest.bytes, xml.len());
    assert_eq!(Some(manifest.sha256), batch.content_hash().ok());
    assert_ne!(
        batch.content_hash(),
        Ok(hex_of(&batch.file())),
        "the hash is of the XML, not the system's own file"
    );

    // A batch whose account lost its bank details is not written at all.
    let mut orphan = batch.clone();
    orphan.ordering = None;
    assert!(orphan.wire().unwrap_err().contains("no bank details"));
    assert!(orphan.content_hash().is_err() && orphan.manifest(String::new(), String::new(), String::new()).is_err());
}

fn hex_of(text: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}
