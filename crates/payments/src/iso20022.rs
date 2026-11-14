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
//!
//! Item IDs travel as the end-to-end reference, so every item ID is held to
//! what ISO 20022 can carry when the item is added to a run, whatever the
//! channel: a run can be moved to another bank after it is built.

use crate::file::{Batch, Direction};

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

/// Whether a value can be an ISO 20022 identifier as it is: 1 to 35
/// printable ASCII characters with no spaces.
pub fn fits_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= MAX_ID && value.chars().all(|c| c.is_ascii_graphic())
}

fn id(what: &str, value: &str) -> Result<String, String> {
    if fits_identifier(value) {
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

/// A recall as `camt.055.001.08`, the customer's request that its bank
/// cancel or recall a payment: the case is the recall's ID, and the
/// underlying payment is named by its batch, end-to-end reference, amount
/// and the day it settled.
pub fn recall_document(recall: &crate::file::Recall, ordering: &OrderingAccount) -> Result<String, String> {
    if recall.amount_minor <= 0 {
        return Err(format!("recall {} has no positive amount", recall.id));
    }
    let case = id("recall ID", &recall.id)?;
    let batch = id("batch ID", &recall.submission)?;
    let item = id("item ID", &recall.item)?;
    let reason = id("reason code", &recall.reason)?;
    let (own, own_agent) = (name(&ordering.name), agent(ordering));
    let currency = format!("{:?}", recall.currency);
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:camt.055.001.08\"><CstmrPmtCxlReq>\
         <Assgnmt><Id>{case}</Id><Assgnr><Pty><Nm>{own}</Nm></Pty></Assgnr><Assgne><Agt>{own_agent}</Agt></Assgne>\
         <CreDtTm>{}T00:00:00</CreDtTm></Assgnmt><Case><Id>{case}</Id><Cretr><Pty><Nm>{own}</Nm></Pty></Cretr></Case>\
         <Undrlyg><OrgnlPmtInfAndCxl><OrgnlPmtInfId>{batch}</OrgnlPmtInfId><TxInf><CxlId>{case}</CxlId>\
         <OrgnlEndToEndId>{item}</OrgnlEndToEndId><OrgnlInstdAmt Ccy=\"{currency}\">{}</OrgnlInstdAmt>\
         <OrgnlReqdExctnDt><Dt>{}</Dt></OrgnlReqdExctnDt><CxlRsnInf><Rsn><Cd>{reason}</Cd></Rsn></CxlRsnInf></TxInf>\
         </OrgnlPmtInfAndCxl></Undrlyg></CstmrPmtCxlReq></Document>\n",
        recall.settled_on,
        amount(recall.amount_minor),
        recall.settled_on,
    ))
}

/// What a bank reads from a payment or collection initiation: enough to
/// check it before acting on any line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Initiation {
    pub message: String,
    pub direction: Direction,
    /// The count and control sum the header declares.
    pub declared: (usize, i64),
    /// Each transaction's end-to-end reference and amount in minor units.
    pub items: Vec<(String, i64)>,
}

fn named<'a, 'i>(node: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|c| c.tag_name().name() == name)
}

fn at<'a, 'i>(node: roxmltree::Node<'a, 'i>, path: &[&str]) -> Option<roxmltree::Node<'a, 'i>> {
    path.iter().try_fold(node, |n, name| named(n, name))
}

fn text_at(node: roxmltree::Node<'_, '_>, path: &[&str]) -> Option<String> {
    at(node, path).and_then(|n| n.text()).map(|t| t.trim().to_string())
}

/// An ISO 20022 amount in minor units. Exactly two decimal places, or none.
pub fn minor(amount: &str) -> Option<i64> {
    let (units, cents) = amount.split_once('.').unwrap_or((amount, "00"));
    if units.is_empty() || cents.len() != 2 || !units.bytes().chain(cents.bytes()).all(|b| b.is_ascii_digit()) {
        return None;
    }
    units.parse::<i64>().ok()?.checked_mul(100)?.checked_add(cents.parse::<i64>().ok()?)
}

fn parse(xml: &str) -> Result<roxmltree::Document<'_>, String> {
    roxmltree::Document::parse(xml).map_err(|e| format!("not well-formed XML: {e}"))
}

/// Read a `pain.001` or `pain.008` initiation as a bank would. Refused with
/// the reason if it is not one or a required element is missing.
pub fn read_initiation(xml: &str) -> Result<Initiation, String> {
    let doc = parse(xml)?;
    let root = doc.root_element();
    let (body, direction, tx) = if let Some(b) = named(root, "CstmrCdtTrfInitn") {
        (b, Direction::Credit, "CdtTrfTxInf")
    } else if let Some(b) = named(root, "CstmrDrctDbtInitn") {
        (b, Direction::Debit, "DrctDbtTxInf")
    } else {
        return Err("not a payment or collection initiation".to_string());
    };
    let missing = |what: &str| format!("the initiation has no readable {what}");
    let message = text_at(body, &["GrpHdr", "MsgId"]).ok_or_else(|| missing("message ID"))?;
    let count = text_at(body, &["GrpHdr", "NbOfTxs"])
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| missing("count"))?;
    let total = text_at(body, &["GrpHdr", "CtrlSum"])
        .and_then(|c| minor(&c))
        .ok_or_else(|| missing("control sum"))?;
    let mut items = Vec::new();
    for info in body.children().filter(|c| c.tag_name().name() == "PmtInf") {
        for t in info.children().filter(|c| c.tag_name().name() == tx) {
            let id = text_at(t, &["PmtId", "EndToEndId"]).ok_or_else(|| missing("end-to-end reference"))?;
            let amount = text_at(t, &["Amt", "InstdAmt"])
                .or_else(|| text_at(t, &["InstdAmt"]))
                .and_then(|a| minor(&a))
                .ok_or_else(|| missing("amount"))?;
            items.push((id, amount));
        }
    }
    Ok(Initiation {
        message,
        direction,
        declared: (count, total),
        items,
    })
}

/// Read a `camt.055` recall as a bank would: its case ID, the payment's
/// end-to-end reference and the amount asked back.
pub fn read_recall(xml: &str) -> Result<(String, String, i64), String> {
    let doc = parse(xml)?;
    let body = named(doc.root_element(), "CstmrPmtCxlReq").ok_or("not a payment cancellation request")?;
    let case = text_at(body, &["Case", "Id"]).ok_or("the request names no case")?;
    let tx = at(body, &["Undrlyg", "OrgnlPmtInfAndCxl", "TxInf"]).ok_or("the request names no payment")?;
    let item = text_at(tx, &["OrgnlEndToEndId"]).ok_or("the request names no payment")?;
    let amount = text_at(tx, &["OrgnlInstdAmt"]).and_then(|a| minor(&a)).ok_or("the request has no amount")?;
    Ok((case, item, amount))
}

/// The return reason an ISO 20022 reason code stands for.
pub fn return_reason(code: &str) -> crate::model::ReturnReason {
    use crate::model::ReturnReason;
    match code {
        "AM04" => ReturnReason::InsufficientFunds,
        "AC04" => ReturnReason::AccountClosed,
        "MD01" | "MD07" => ReturnReason::AuthorityCancelled,
        "AC01" | "AC03" => ReturnReason::InvalidAccount,
        other => ReturnReason::Other(other.to_string()),
    }
}

/// The ISO 20022 reason code for a return reason: what a bank writes.
pub fn reason_code(reason: &crate::model::ReturnReason) -> String {
    use crate::model::ReturnReason;
    match reason {
        ReturnReason::InsufficientFunds => "AM04".to_string(),
        ReturnReason::AccountClosed => "AC04".to_string(),
        ReturnReason::AuthorityCancelled => "MD01".to_string(),
        ReturnReason::InvalidAccount => "AC01".to_string(),
        ReturnReason::Other(code) => code.clone(),
    }
}

/// A status reason: its code, and the bank's words after it where given.
fn status_reason(node: roxmltree::Node<'_, '_>) -> String {
    let code = text_at(node, &["StsRsnInf", "Rsn", "Cd"]).unwrap_or_else(|| "NARR".to_string());
    match text_at(node, &["StsRsnInf", "AddtlInf"]) {
        Some(words) => format!("{code}: {words}"),
        None => code,
    }
}

fn date_at(node: roxmltree::Node<'_, '_>, path: &[&str]) -> Option<chrono::NaiveDate> {
    text_at(node, path).and_then(|d| d.get(..10)?.parse().ok())
}

/// A signed amount: a debit is money out of the insurer's account.
fn signed(node: roxmltree::Node<'_, '_>) -> Option<i64> {
    let amount = text_at(node, &["Amt"]).and_then(|a| minor(&a))?;
    match text_at(node, &["CdtDbtInd"]).as_deref() {
        Some("CRDT") => Some(amount),
        Some("DBIT") => Some(-amount),
        _ => None,
    }
}

/// What a bank sends back, as the messages the module records: a
/// `pain.002` status report becomes an acknowledgement; a `camt.054`
/// notification becomes the settlements and returns it reports; a
/// `camt.053` statement becomes those and then the statement itself, with a
/// batch booked under its payment information ID and a return under the
/// item's end-to-end reference, as the module expects to see them.
/// `accounts` gives the module's account code for each account number the
/// bank names. Refused whole, with the reason, if anything in it cannot be
/// read: a statement half read is a statement that does not balance.
pub fn read_messages(xml: &str, accounts: &std::collections::BTreeMap<String, String>) -> Result<Vec<crate::file::BankMessage>, String> {
    use crate::file::BankMessage;
    let doc = parse(xml)?;
    let root = doc.root_element();
    if let Some(report) = named(root, "CstmrPmtStsRpt") {
        let group = named(report, "OrgnlGrpInfAndSts").ok_or("a status report names the message it reports on")?;
        let id = text_at(group, &["OrgnlMsgId"]).ok_or("a status report names the message it reports on")?;
        let refused = (text_at(group, &["GrpSts"]).as_deref() == Some("RJCT")).then(|| status_reason(group));
        let mut rejected = std::collections::BTreeMap::new();
        for info in report.children().filter(|c| c.tag_name().name() == "OrgnlPmtInfAndSts") {
            for tx in info.children().filter(|c| c.tag_name().name() == "TxInfAndSts") {
                if text_at(tx, &["TxSts"]).as_deref() == Some("RJCT") {
                    let item = text_at(tx, &["OrgnlEndToEndId"]).ok_or("a rejected transaction names its end-to-end reference")?;
                    rejected.insert(item, status_reason(tx));
                }
            }
        }
        return Ok(vec![BankMessage::Acknowledgement { id, refused, rejected }]);
    }
    if let Some(resolution) = named(root, "RsltnOfInvstgtn") {
        // A `camt.029` answering a recall: confirmed (`CNCL`), or refused
        // with the reason the bank gives.
        let recall = text_at(resolution, &["Case", "Id"]).ok_or("a resolution names the case it answers")?;
        let status = text_at(resolution, &["Sts", "Conf"]).ok_or("a resolution says how the case was resolved")?;
        let accepted = status == "CNCL";
        let reason = (!accepted).then(|| text_at(resolution, &["CxlDtls", "TxInfAndSts", "CxlStsRsnInf", "Rsn", "Cd"]).unwrap_or(status));
        return Ok(vec![BankMessage::RecallAnswer { recall, accepted, reason }]);
    }
    let (body, part, statement) = if let Some(b) = named(root, "BkToCstmrStmt") {
        (b, "Stmt", true)
    } else if let Some(b) = named(root, "BkToCstmrDbtCdtNtfctn") {
        (b, "Ntfctn", false)
    } else {
        return Err("not a status report, statement, notification or resolution".to_string());
    };
    let mut out = Vec::new();
    for report in body.children().filter(|c| c.tag_name().name() == part) {
        let number = text_at(report, &["Acct", "Id", "Othr", "Id"]).ok_or("a report names its account")?;
        let account = accounts.get(&number).cloned().ok_or_else(|| format!("account {number} is not one of ours"))?;
        let mut lines = Vec::new();
        for (n, entry) in report.children().filter(|c| c.tag_name().name() == "Ntry").enumerate() {
            let unreadable = || format!("entry {} on account {number} cannot be read", n + 1);
            let amount = signed(entry).ok_or_else(unreadable)?;
            let booked = date_at(entry, &["BookgDt", "Dt"]).ok_or_else(unreadable)?;
            let line_id = text_at(entry, &["NtryRef"])
                .or_else(|| text_at(entry, &["AcctSvcrRef"]))
                .unwrap_or_else(|| format!("{number}-{booked}-{}", n + 1));
            let tx = at(entry, &["NtryDtls", "TxDtls"]);
            let end_to_end = tx.and_then(|t| text_at(t, &["Refs", "EndToEndId"]));
            let reference = if let Some(batch) = text_at(entry, &["NtryDtls", "Btch", "PmtInfId"]) {
                out.push(BankMessage::Settlement {
                    id: batch.clone(),
                    settled_on: booked,
                });
                batch
            } else if let Some(returned) = tx.and_then(|t| named(t, "RtrInf")) {
                let item = end_to_end.clone().ok_or_else(unreadable)?;
                let code = text_at(returned, &["Rsn", "Cd"]).ok_or_else(unreadable)?;
                out.push(BankMessage::Return {
                    item: item.clone(),
                    reason: return_reason(&code),
                    returned_on: booked,
                });
                item
            } else {
                tx.and_then(|t| text_at(t, &["RmtInf", "Ustrd"])).or(end_to_end).unwrap_or_default()
            };
            lines.push(crate::model::StatementLine {
                id: line_id,
                amount_minor: amount,
                reference,
                payer_account: tx.and_then(|t| text_at(t, &["RltdPties", "DbtrAcct", "Id", "Othr", "Id"])),
            });
        }
        if statement {
            let balance = |code: &str| {
                report
                    .children()
                    .filter(|c| c.tag_name().name() == "Bal")
                    .find(|b| text_at(*b, &["Tp", "CdOrPrtry", "Cd"]).as_deref() == Some(code))
            };
            let (opening, closing) = match (balance("OPBD"), balance("CLBD")) {
                (Some(o), Some(c)) => (o, c),
                _ => return Err(format!("the statement for account {number} has no opening or no closing balance")),
            };
            let (Some(opening_minor), Some(closing_minor), Some(date)) = (signed(opening), signed(closing), date_at(closing, &["Dt", "Dt"])) else {
                return Err(format!("the balances on the statement for account {number} cannot be read"));
            };
            out.push(BankMessage::Statement {
                account,
                date,
                opening_minor,
                closing_minor,
                lines,
            });
        }
    }
    Ok(out)
}
