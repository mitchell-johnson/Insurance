//! The ISO 20022 writer: what a bank would be handed for a batch, and what
//! the writer refuses to hand over.

use chrono::NaiveDate;
use ic_money::Currency;
use ic_payments::config::WireFormat;
use ic_payments::file::{Batch, BatchItem, Direction};
use ic_payments::iso20022::{document, minor, read_initiation, OrderingAccount};

fn item(id: &str, name: &str, number: &str, amount_minor: i64) -> BatchItem {
    BatchItem {
        item: id.into(),
        name: name.into(),
        number: number.into(),
        amount_minor,
    }
}

fn batch(direction: Direction, items: Vec<BatchItem>) -> Batch {
    Batch {
        id: "SUB-0001".into(),
        channel: "bank-a-file".into(),
        account: "claims".into(),
        direction,
        value_date: NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
        currency: Currency::NZD,
        items,
        format: WireFormat::Iso20022,
        ordering: Some(ordering(Some("BKNZNZ22"))),
    }
}

fn ordering(bic: Option<&str>) -> OrderingAccount {
    OrderingAccount {
        name: "Example Life Ltd".into(),
        number: "02-0100-0123456-000".into(),
        bic: bic.map(str::to_string),
    }
}

/// Purpose: pin the whole credit transfer message for a small batch, byte
/// for byte.
/// Why it matters: the file's hash is its identity at the bank and in the
/// evidence store; a change to a single element is a different file, and a
/// wrong count or control sum is a file the bank rejects whole.
#[test]
fn a_payment_batch_is_a_credit_transfer_initiation() {
    let batch = batch(
        Direction::Credit,
        vec![
            item("PAY-1", "A Payee", "01-0001-0000001-00", 12_345),
            item("PAY-2", "B Payee", "01-0001-0000002-00", 5),
        ],
    );
    let xml = document(&batch, &ordering(Some("BKNZNZ22"))).unwrap();
    let expected = concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\"><CstmrCdtTrfInitn>",
        "<GrpHdr><MsgId>SUB-0001</MsgId><CreDtTm>2026-11-13T00:00:00</CreDtTm><NbOfTxs>2</NbOfTxs><CtrlSum>123.50</CtrlSum>",
        "<InitgPty><Nm>Example Life Ltd</Nm></InitgPty></GrpHdr>",
        "<PmtInf><PmtInfId>SUB-0001</PmtInfId><PmtMtd>TRF</PmtMtd><BtchBookg>true</BtchBookg><NbOfTxs>2</NbOfTxs><CtrlSum>123.50</CtrlSum>",
        "<ReqdExctnDt><Dt>2026-11-13</Dt></ReqdExctnDt>",
        "<Dbtr><Nm>Example Life Ltd</Nm></Dbtr><DbtrAcct><Id><Othr><Id>02-0100-0123456-000</Id></Othr></Id></DbtrAcct>",
        "<DbtrAgt><FinInstnId><BICFI>BKNZNZ22</BICFI></FinInstnId></DbtrAgt>",
        "<CdtTrfTxInf><PmtId><EndToEndId>PAY-1</EndToEndId></PmtId><Amt><InstdAmt Ccy=\"NZD\">123.45</InstdAmt></Amt>",
        "<Cdtr><Nm>A Payee</Nm></Cdtr><CdtrAcct><Id><Othr><Id>01-0001-0000001-00</Id></Othr></Id></CdtrAcct></CdtTrfTxInf>",
        "<CdtTrfTxInf><PmtId><EndToEndId>PAY-2</EndToEndId></PmtId><Amt><InstdAmt Ccy=\"NZD\">0.05</InstdAmt></Amt>",
        "<Cdtr><Nm>B Payee</Nm></Cdtr><CdtrAcct><Id><Othr><Id>01-0001-0000002-00</Id></Othr></Id></CdtrAcct></CdtTrfTxInf>",
        "</PmtInf></CstmrCdtTrfInitn></Document>\n",
    );
    assert_eq!(xml, expected);
    assert_eq!(document(&batch, &ordering(Some("BKNZNZ22"))).unwrap(), xml, "the same batch is the same bytes");
}

/// Purpose: a collection batch is written as a direct debit initiation, with
/// the insurer as creditor and each payer as debtor, and no BIC where none
/// is configured.
/// Why it matters: a collection written as a payment would send money out
/// of the account it was meant to arrive in.
#[test]
fn a_collection_batch_is_a_direct_debit_initiation() {
    let batch = batch(Direction::Debit, vec![item("DD-1", "A Payer", "03-0003-0000003-00", 4_000)]);
    let xml = document(&batch, &ordering(None)).unwrap();
    assert!(xml.contains("pain.008.001.08\"><CstmrDrctDbtInitn>"));
    assert!(xml.contains("<PmtMtd>DD</PmtMtd>"));
    assert!(xml.contains("<ReqdColltnDt>2026-11-13</ReqdColltnDt>"));
    assert!(xml.contains("<Cdtr><Nm>Example Life Ltd</Nm></Cdtr><CdtrAcct><Id><Othr><Id>02-0100-0123456-000</Id></Othr></Id></CdtrAcct>"));
    assert!(xml.contains("<CdtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></CdtrAgt>"));
    assert!(xml.contains(concat!(
        "<DrctDbtTxInf><PmtId><EndToEndId>DD-1</EndToEndId></PmtId><InstdAmt Ccy=\"NZD\">40.00</InstdAmt>",
        "<DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></DbtrAgt>",
        "<Dbtr><Nm>A Payer</Nm></Dbtr><DbtrAcct><Id><Othr><Id>03-0003-0000003-00</Id></Othr></Id></DbtrAcct></DrctDbtTxInf>"
    )));
    assert!(!xml.contains("CdtTrfTxInf"));
}

/// Purpose: a name with markup characters, a control character, or more
/// than 140 characters is still written as well-formed text of legal length.
/// Why it matters: names come from people. One ampersand must not make the
/// whole file unreadable to the bank, or let a name add elements to it.
#[test]
fn names_cannot_break_the_message() {
    let hostile = "O'Brien & <Sons> \"Trust\"\tLtd";
    let long = "x".repeat(200);
    let batch = batch(Direction::Credit, vec![item("PAY-1", hostile, "01-1", 100), item("PAY-2", &long, "01-2", 100)]);
    let xml = document(&batch, &ordering(None)).unwrap();
    assert!(xml.contains("<Nm>O&apos;Brien &amp; &lt;Sons&gt; &quot;Trust&quot; Ltd</Nm>"));
    assert!(xml.contains(&format!("<Nm>{}</Nm>", "x".repeat(140))));
    assert!(!xml.contains(&"x".repeat(141)));
}

/// Purpose: a batch that cannot be written faithfully is refused with the
/// reason: no items, an identifier too long or with a space, or an amount
/// that is not positive.
/// Why it matters: the end-to-end reference is how a settlement or return
/// finds its item again. A shortened reference would match nothing, or the
/// wrong item, so refusing before anything is sent is the only safe answer.
#[test]
fn a_batch_that_cannot_be_written_faithfully_is_refused() {
    let own = ordering(None);
    let empty = document(&batch(Direction::Credit, vec![]), &own).unwrap_err();
    assert_eq!(empty, "batch SUB-0001 has no items");

    let long = "P".repeat(36);
    let too_long = document(&batch(Direction::Credit, vec![item(&long, "A", "01-1", 100)]), &own).unwrap_err();
    assert!(too_long.starts_with("item ID") && too_long.contains("1 to 35"), "{too_long}");
    assert!(document(&batch(Direction::Credit, vec![item("PAY 1", "A", "01-1", 100)]), &own).is_err());
    assert!(document(&batch(Direction::Credit, vec![item("", "A", "01-1", 100)]), &own).is_err());

    let mut spaced = batch(Direction::Debit, vec![item("DD-1", "A", "01-1", 100)]);
    spaced.id = "SUB 0001".into();
    assert!(document(&spaced, &own).unwrap_err().starts_with("batch ID"));

    for amount in [0, -100] {
        let refused = document(&batch(Direction::Credit, vec![item("PAY-1", "A", "01-1", amount)]), &own).unwrap_err();
        assert_eq!(refused, "item PAY-1 has no positive amount");
    }
}

/// Purpose: what the writer produces, a bank's reader reads back as the
/// same message ID, direction, count, control sum and transactions; a
/// document that is not XML, not an initiation, or missing a required
/// element is refused with the reason.
/// Why it matters: the test bank checks submissions with this reader. If it
/// read the writer's output differently from what the writer meant, every
/// end-to-end test of an ISO 20022 channel would prove nothing.
#[test]
fn a_bank_reads_back_what_was_written() {
    let items = vec![item("PAY-1", "A", "01-1", 12_345), item("PAY-2", "B", "01-2", 5)];
    for direction in [Direction::Credit, Direction::Debit] {
        let xml = document(&batch(direction, items.clone()), &ordering(None)).unwrap();
        let read = read_initiation(&xml).unwrap();
        assert_eq!(read.message, "SUB-0001");
        assert_eq!(read.direction, direction);
        assert_eq!(read.declared, (2, 12_350));
        assert_eq!(read.items, vec![("PAY-1".to_string(), 12_345), ("PAY-2".to_string(), 5)]);
    }
    assert!(read_initiation("not xml").unwrap_err().starts_with("not well-formed XML"));
    assert_eq!(read_initiation("<Document/>").unwrap_err(), "not a payment or collection initiation");
    let xml = document(&batch(Direction::Credit, items), &ordering(None)).unwrap();
    for (from, to, what) in [
        ("<MsgId>SUB-0001</MsgId>", "", "message ID"),
        (
            "<NbOfTxs>2</NbOfTxs><CtrlSum>123.50</CtrlSum><InitgPty>",
            "<NbOfTxs>two</NbOfTxs><CtrlSum>123.50</CtrlSum><InitgPty>",
            "count",
        ),
        ("<CtrlSum>123.50</CtrlSum><InitgPty>", "<CtrlSum>123.5</CtrlSum><InitgPty>", "control sum"),
        ("<EndToEndId>PAY-1</EndToEndId>", "", "end-to-end reference"),
        (">123.45<", ">1,23.45<", "amount"),
    ] {
        let broken = xml.replacen(from, to, 1);
        assert_ne!(broken, xml, "{what}");
        assert_eq!(read_initiation(&broken).unwrap_err(), format!("the initiation has no readable {what}"));
    }
}

/// Purpose: an ISO 20022 amount reads as minor units only when it has two
/// decimal places or none, digits only, and fits.
/// Why it matters: "12.5" read as 12.05, or a minus sign passed over, would
/// be the wrong amount accepted silently.
#[test]
fn amounts_are_read_exactly_or_not_at_all() {
    assert_eq!(minor("123.45"), Some(12_345));
    assert_eq!(minor("7"), Some(700));
    assert_eq!(minor("0.05"), Some(5));
    for bad in ["12.5", "12.345", "-1.00", ".50", "1e3", "", "99999999999999999999.00"] {
        assert_eq!(minor(bad), None, "{bad}");
    }
}
