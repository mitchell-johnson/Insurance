//! ISO 20022 customer-to-bank messages for a batch: `pain.001.001.09` for
//! payments and `pain.008.001.08` for direct debit collections.
//!
//! The design notes settle a file channel and an API channel with two banks
//! but name no bank and no format. ISO 20022 is what a bank's host-to-host
//! service most commonly accepts, so it is the format built here; a bank
//! that wants its own layout gets its own writer beside this one.
//!
//! The text is fixed by the batch and the ordering account alone: the same
//! batch is the same bytes on every attempt, so its hash identifies it. For
//! that reason the creation time in the header is the value date at
//! midnight, not the wall clock.
//!
//! New Zealand account numbers are not IBANs, so accounts are written as
//! `Othr/Id`. The bank is identified by BIC where one is configured.

use ic_payments::file::{Batch, Direction};

/// The insurer's own account a batch pays from or collects into, as the
/// bank knows it. Held with the channel's settings, not in the module: the
/// module knows the account only by its code.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OrderingAccount {
    /// The account holder's name as the bank has it.
    pub name: String,
    pub number: String,
    /// The bank's BIC, if it wants one in the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bic: Option<String>,
}

/// The longest identifier ISO 20022 carries (`Max35Text`).
const MAX_ID: usize = 35;
/// The longest name (`Max140Text`).
const MAX_NAME: usize = 140;

/// Text safe inside an element: the five characters XML reserves are
/// escaped and control characters become spaces.
fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// A name cut to what the message can carry. A long name is shortened; an
/// identifier never is, because a shortened identifier is a different one.
fn name(value: &str) -> String {
    text(&value.chars().take(MAX_NAME).collect::<String>())
}

fn id(what: &str, value: &str) -> Result<String, String> {
    let fits = !value.is_empty() && value.chars().count() <= MAX_ID && value.chars().all(|c| c.is_ascii_graphic());
    if fits {
        Ok(text(value))
    } else {
        Err(format!(
            "{what} {value:?} cannot be an ISO 20022 identifier: 1 to {MAX_ID} printable characters with no spaces"
        ))
    }
}

/// An amount in minor units as ISO 20022 writes it. Every currency this
/// system holds accounts in has two decimal places.
fn amount(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

fn agent(account: &OrderingAccount) -> String {
    match &account.bic {
        Some(bic) => format!("<FinInstnId><BICFI>{}</BICFI></FinInstnId>", text(bic)),
        None => "<FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId>".to_string(),
    }
}

fn account_id(number: &str) -> String {
    format!("<Id><Othr><Id>{}</Id></Othr></Id>", text(number))
}

/// The batch as the ISO 20022 message for its direction. Refused, with the
/// reason, if the batch cannot be written faithfully: an identifier too
/// long to carry, an amount that is not positive, or no items.
pub fn document(batch: &Batch, ordering: &OrderingAccount) -> Result<String, String> {
    if batch.items.is_empty() {
        return Err(format!("batch {} has no items", batch.id));
    }
    let message = id("batch ID", &batch.id)?;
    let currency = format!("{:?}", batch.currency);
    let count = batch.items.len();
    let total = amount(batch.total_minor());
    let date = batch.value_date;
    let mut transactions = String::new();
    for item in &batch.items {
        if item.amount_minor <= 0 {
            return Err(format!("item {} has no positive amount", item.item));
        }
        let end_to_end = id("item ID", &item.item)?;
        let instructed = format!("<InstdAmt Ccy=\"{currency}\">{}</InstdAmt>", amount(item.amount_minor));
        let (party, party_account) = (name(&item.name), account_id(&item.number));
        transactions.push_str(&match batch.direction {
            Direction::Credit => format!(
                "<CdtTrfTxInf><PmtId><EndToEndId>{end_to_end}</EndToEndId></PmtId><Amt>{instructed}</Amt>\
                 <Cdtr><Nm>{party}</Nm></Cdtr><CdtrAcct>{party_account}</CdtrAcct></CdtTrfTxInf>"
            ),
            Direction::Debit => format!(
                "<DrctDbtTxInf><PmtId><EndToEndId>{end_to_end}</EndToEndId></PmtId>{instructed}\
                 <DbtrAgt><FinInstnId><Othr><Id>NOTPROVIDED</Id></Othr></FinInstnId></DbtrAgt>\
                 <Dbtr><Nm>{party}</Nm></Dbtr><DbtrAcct>{party_account}</DbtrAcct></DrctDbtTxInf>"
            ),
        });
    }
    let header = format!(
        "<GrpHdr><MsgId>{message}</MsgId><CreDtTm>{date}T00:00:00</CreDtTm><NbOfTxs>{count}</NbOfTxs><CtrlSum>{total}</CtrlSum>\
         <InitgPty><Nm>{}</Nm></InitgPty></GrpHdr>",
        name(&ordering.name)
    );
    let (own, own_account, own_agent) = (name(&ordering.name), account_id(&ordering.number), agent(ordering));
    let totals = format!("<BtchBookg>true</BtchBookg><NbOfTxs>{count}</NbOfTxs><CtrlSum>{total}</CtrlSum>");
    Ok(match batch.direction {
        Direction::Credit => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.001.001.09\"><CstmrCdtTrfInitn>{header}\
             <PmtInf><PmtInfId>{message}</PmtInfId><PmtMtd>TRF</PmtMtd>{totals}<ReqdExctnDt><Dt>{date}</Dt></ReqdExctnDt>\
             <Dbtr><Nm>{own}</Nm></Dbtr><DbtrAcct>{own_account}</DbtrAcct><DbtrAgt>{own_agent}</DbtrAgt>{transactions}</PmtInf>\
             </CstmrCdtTrfInitn></Document>\n"
        ),
        Direction::Debit => format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:pain.008.001.08\"><CstmrDrctDbtInitn>{header}\
             <PmtInf><PmtInfId>{message}</PmtInfId><PmtMtd>DD</PmtMtd>{totals}<ReqdColltnDt>{date}</ReqdColltnDt>\
             <Cdtr><Nm>{own}</Nm></Cdtr><CdtrAcct>{own_account}</CdtrAcct><CdtrAgt>{own_agent}</CdtrAgt>{transactions}</PmtInf>\
             </CstmrDrctDbtInitn></Document>\n"
        ),
    })
}
