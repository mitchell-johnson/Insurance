//! The forwarder: what the commission module knows that the channels need,
//! carried across by the portal service.
//!
//! The commission module (`distribution` on the nodes) owns advisers'
//! engagements, licence status and held money. The channels show them: the
//! adviser's read-only state, the customer's "adviser unavailable", the
//! payee's own held balance and the held balance workbench. Neither module
//! depends on the other's crate. This loop asks that module which parties
//! changed, reads each one's position and engagements as JSON, and proposes
//! the channel commands that record them.
//!
//! Every command it sends is safe to send again: a position replaces the
//! last one, and a licence or status change already recorded is ignored. So
//! the cursor lives in memory, a restart republishes everything once, and
//! two copies of the service doing the same work do no harm.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::NaiveDate;
use ic_core::PartyId;
use ic_raft::ClusterClient;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::directory::{Actor, DirectoryCommand, Engagement, LicenceStatus, StructureLink};
use crate::http::Channel;
use crate::query::Question;
use crate::staff::At;
use crate::state::{Command, Envelope};
use crate::workbench::{Action, Document, HoldReason, Item, ItemKind, PartyStatus, Proposal, WorkbenchCommand};

/// The commission module's name on the nodes.
pub const COMMISSION_MODULE: &str = "distribution";
/// Who the forwarder speaks as when it publishes money.
pub const PUBLISHER: &str = "commission";
/// Who it speaks as for engagements, licences and status.
pub const STRUCTURE: &str = "distribution";

/// How an error begins when the module was reached and would not answer,
/// as against not being reached at all.
pub const REFUSED: &str = "refused: ";

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

/// Another module on the nodes: somewhere a question can be put and a command sent.
pub trait Source: Send + Sync {
    fn ask(&self, question: Value) -> Asked<'_>;
    /// Send one command envelope. The answer is that module's output.
    fn tell(&self, envelope: Value) -> Asked<'_>;
}

/// A module on the ledger nodes.
pub struct Hosted {
    pub client: ClusterClient,
    /// The module's name there.
    pub module: &'static str,
    /// How long a command looks for a leader before giving up.
    pub patience: Duration,
}

impl Source for Hosted {
    fn ask(&self, question: Value) -> Asked<'_> {
        Box::pin(async move {
            match self.client.query_module(self.module, &question).await {
                Ok(Ok(answer)) => Ok(answer),
                Ok(Err(refusal)) => Err(format!("{REFUSED}{refusal}")),
                Err(e) => Err(e.to_string()),
            }
        })
    }

    fn tell(&self, envelope: Value) -> Asked<'_> {
        Box::pin(async move {
            match self.client.propose_module_until(self.module, &envelope, self.patience).await {
                Ok(reply) => reply.result.map_err(|refusal| format!("{REFUSED}{refusal}")),
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

// What the forwarder reads of the commission module's answers. Fields it
// does not use are left out, so that module can add to its answers freely.

#[derive(Debug, Deserialize)]
struct Changed {
    seq: u64,
    parties: Vec<PartyId>,
}

#[derive(Debug, Deserialize)]
struct Position {
    summary: Summary,
    held: Vec<HeldLine>,
    #[serde(default)]
    documents: Vec<Filed>,
    #[serde(default)]
    licence_history: Vec<Licence>,
    #[serde(default)]
    ceased: Option<(NaiveDate, String)>,
    #[serde(default)]
    open_cases: Vec<String>,
    #[serde(default)]
    memo: Vec<Memo>,
}

/// A debt another module keeps in its own ledger, such as a claim overpayment.
#[derive(Debug, Deserialize)]
struct Memo {
    owner: String,
    reference: String,
    outstanding_minor: i64,
    as_at: NaiveDate,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Summary {
    receivable_minor: i64,
    advance_minor: i64,
}

#[derive(Debug, Deserialize)]
struct HeldLine {
    period: String,
    book: Option<String>,
    reason: Option<HoldReason>,
    amount_minor: i64,
}

#[derive(Debug, Deserialize)]
struct Filed {
    kind: String,
    document_ref: String,
    on: NaiveDate,
}

#[derive(Debug, Deserialize)]
struct Licence {
    state: LicenceStatus,
    effective: NaiveDate,
    source_ref: String,
}

#[derive(Debug, Deserialize)]
struct Engagements {
    engagements: Vec<Engaged>,
}

#[derive(Debug, Deserialize)]
struct Engaged {
    id: String,
    adviser: PartyId,
    with: PartyId,
    from: NaiveDate,
    to: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
struct Relation {
    child: PartyId,
    parent: PartyId,
    kind: String,
    from: NaiveDate,
    to: Option<NaiveDate>,
}

#[derive(Debug, Deserialize)]
struct ConductCase {
    case: String,
    party: PartyId,
    #[serde(default)]
    advisers: Vec<PartyId>,
    #[serde(default)]
    payees: Vec<PartyId>,
    #[serde(default)]
    closed: Option<Value>,
}

fn read<T: serde::de::DeserializeOwned>(what: &str, answer: Value) -> Result<T, String> {
    serde_json::from_value(answer).map_err(|e| format!("the commission module's {what} answer was not understood: {e}"))
}

/// The first day of a `YYYY-MM` period, which is when a held line began to age.
fn period_start(period: &str, fallback: NaiveDate) -> NaiveDate {
    NaiveDate::parse_from_str(&format!("{period}-01"), "%Y-%m-%d").unwrap_or(fallback)
}

/// The channel commands that record one party as the commission module
/// describes it on `today`, each with the module it is sent as.
pub fn commands_for(
    party: &PartyId,
    position: Value,
    engagements: Value,
    today: NaiveDate,
    currency: &str,
    conduct: Vec<String>,
) -> Result<Vec<(&'static str, Command)>, String> {
    let position: Position = read("position", position)?;
    let engagements: Engagements = read("engagements", engagements)?;
    let mut out = Vec::new();

    // Who they work with, before what follows from it.
    for e in engagements.engagements {
        out.push((
            STRUCTURE,
            Command::Directory(DirectoryCommand::RecordEngagement {
                engagement: Engagement {
                    id: e.id,
                    adviser: e.adviser,
                    fap: e.with,
                    from: e.from,
                    to: e.to,
                },
            }),
        ));
    }
    for l in &position.licence_history {
        out.push((
            STRUCTURE,
            Command::Directory(DirectoryCommand::RecordLicence {
                party: party.clone(),
                status: l.state,
                effective: l.effective,
                source: l.source_ref.clone(),
            }),
        ));
    }

    // The party's standing on the workbench: ceased outranks any licence.
    let licence = position.licence_history.iter().filter(|l| l.effective <= today).max_by_key(|l| l.effective);
    let status = match (&position.ceased, licence) {
        (Some((on, reason)), _) => Some((
            match reason.as_str() {
                "deceased" => PartyStatus::Deceased,
                "liquidation" => PartyStatus::InLiquidation,
                _ => PartyStatus::RunOff,
            },
            *on,
            format!("ceased: {reason}"),
        )),
        (None, Some(l)) => Some((
            match l.state {
                LicenceStatus::Active => PartyStatus::Normal,
                LicenceStatus::Suspended => PartyStatus::LicenceSuspended,
                LicenceStatus::Revoked | LicenceStatus::Lapsed => PartyStatus::LicenceRevoked,
            },
            l.effective,
            l.source_ref.clone(),
        )),
        (None, None) => None,
    };
    if let Some((status, effective, source)) = status {
        out.push((
            STRUCTURE,
            Command::Workbench(WorkbenchCommand::RecordStatus {
                party: party.clone(),
                status,
                effective,
                source,
            }),
        ));
    }

    // The money. A held line with no reason recorded is held all the same;
    // the workbench shows it as a legal hold until the owner says otherwise.
    let mut items: Vec<Item> = position
        .held
        .iter()
        .filter(|line| line.amount_minor > 0)
        .map(|line| Item {
            id: format!(
                "held/{}/{}/{}",
                line.period,
                line.book.as_deref().unwrap_or("-"),
                line.reason.map_or("unstated", HoldReason::key)
            ),
            kind: ItemKind::Held,
            amount_minor: line.amount_minor,
            currency: currency.to_string(),
            period: Some(line.period.clone()),
            book: line.book.clone(),
            reason: Some(line.reason.unwrap_or(HoldReason::LegalHold)),
            since: period_start(&line.period, today),
            status: None,
        })
        .collect();
    for (id, kind, owed) in [
        ("receivable", ItemKind::ClawbackReceivable, position.summary.receivable_minor),
        ("advance", ItemKind::Advance, position.summary.advance_minor),
    ] {
        if owed != 0 {
            items.push(Item {
                id: id.to_string(),
                kind,
                // The commission module states what the party owes as a positive figure; the workbench as a negative one.
                amount_minor: -owed.abs(),
                currency: currency.to_string(),
                period: None,
                book: None,
                reason: None,
                since: today,
                status: None,
            });
        }
    }
    // What the party owes in another module's ledger, so the approver sees the whole of it. It is worked in
    // the module that owns it; an action against it here is refused by the commission module.
    for m in position.memo.iter().filter(|m| m.outstanding_minor != 0) {
        items.push(Item {
            id: format!("memo/{}/{}", m.owner, m.reference),
            kind: ItemKind::Overpayment,
            amount_minor: -m.outstanding_minor.abs(),
            currency: currency.to_string(),
            period: None,
            book: None,
            reason: None,
            since: m.as_at,
            status: m.status.clone(),
        });
    }
    out.push((
        PUBLISHER,
        Command::Workbench(WorkbenchCommand::Publish {
            party: party.clone(),
            items,
            documents: position
                .documents
                .into_iter()
                .map(|d| Document {
                    reference: d.document_ref,
                    kind: d.kind,
                    filed: d.on,
                })
                .collect(),
            cases: position.open_cases,
            conduct: Some(conduct),
        }),
    ));
    Ok(out)
}

/// The conduct cases open on each party: the body whose licence event opened
/// the case, and every adviser and payee it reaches.
pub fn conduct_by_party(cases: Value) -> Result<BTreeMap<PartyId, Vec<String>>, String> {
    let cases: Vec<ConductCase> = read("conduct_cases", cases)?;
    let mut out: BTreeMap<PartyId, Vec<String>> = BTreeMap::new();
    for c in cases.into_iter().filter(|c| c.closed.as_ref().is_none_or(Value::is_null)) {
        let reached: BTreeSet<PartyId> = std::iter::once(c.party).chain(c.advisers).chain(c.payees).collect();
        for party in reached {
            out.entry(party).or_default().push(c.case.clone());
        }
    }
    Ok(out)
}

/// The structure above the advice businesses, as directory commands: who
/// sits under whom and who operates under whose licence. `only` keeps the
/// links that touch one of those parties. Who is paid for whom is not
/// structure a licence follows, and is left out.
pub fn structure_for(relations: Value, only: Option<&[PartyId]>) -> Result<Vec<Command>, String> {
    let relations: Vec<Relation> = read("relations", relations)?;
    Ok(relations
        .into_iter()
        .filter(|r| matches!(r.kind.as_str(), "under" | "licensed_by"))
        .filter(|r| only.is_none_or(|parties| parties.contains(&r.child) || parties.contains(&r.parent)))
        .map(|r| {
            Command::Directory(DirectoryCommand::RecordStructure {
                child: r.child,
                link: StructureLink {
                    parent: r.parent,
                    from: r.from,
                    to: r.to,
                },
            })
        })
        .collect())
}

async fn send(channel: &dyn Channel, key: String, at: At, module: &str, command: Command, about: &str) -> Result<(), String> {
    let envelope = Envelope {
        key,
        at,
        actor: Actor::Module(module.to_string()),
        command,
    };
    let output = channel.propose(envelope).await.map_err(|e| e.0)?;
    output
        .result
        .map(|_| ())
        .map_err(|refusal| format!("the channel module refused what was forwarded for {about}: {refusal:?}"))
}

/// One pass: forward every party changed since `cursor`. Returns the cursor
/// to ask from next time, which moves only when the whole pass succeeded.
pub async fn pass(source: &dyn Source, channel: &dyn Channel, cursor: u64, at: At, currency: &str) -> Result<u64, String> {
    let changed: Changed = read("changed_parties", source.ask(json!({ "changed_parties": { "since": cursor } })).await?)?;
    if changed.parties.is_empty() {
        return Ok(changed.seq);
    }
    let today = source.ask(json!("status")).await?["today"]
        .as_str()
        .and_then(|d| d.parse::<NaiveDate>().ok())
        .ok_or("the commission module has no business date yet")?;
    // The structure first, so a licence lost above a provider reaches the advisers under it. A fresh
    // forwarder sends all of it; after that, the links that touch a party that changed.
    let relations = source.ask(json!("relations")).await?;
    let only = (cursor > 0).then_some(changed.parties.as_slice());
    for (n, command) in structure_for(relations, only)?.into_iter().enumerate() {
        send(
            channel,
            format!("forward:structure:{}:{n}", changed.seq),
            at,
            STRUCTURE,
            command,
            "the structure",
        )
        .await?;
    }
    let mut conduct = conduct_by_party(source.ask(json!("conduct_cases")).await?)?;
    for party in &changed.parties {
        let position = source.ask(json!({ "position": { "party": party, "as_at": today } })).await?;
        let engagements = source.ask(json!({ "engagements": { "adviser": party, "as_at": today } })).await?;
        let open = conduct.remove(party).unwrap_or_default();
        for (n, (module, command)) in commands_for(party, position, engagements, today, currency, open)?.into_iter().enumerate() {
            send(channel, format!("forward:{party}:{}:{n}", changed.seq), at, module, command, &party.to_string()).await?;
        }
    }
    Ok(changed.seq)
}

/// A workbench action in the commission module's own terms.
fn their_action(action: &Action, document: &str) -> Value {
    // A receivable named `commission` is the party's commission debt; anything else is one another module raised.
    let debt = |receivable: &str| if receivable == "commission" { "commission" } else { "receivable" };
    match action {
        Action::Release { amount_minor, account, .. } => json!({ "release": { "amount_minor": amount_minor, "account_ref": account } }),
        Action::Offset { amount_minor, receivable } => json!({ "offset": { "amount_minor": amount_minor, "against": debt(receivable) } }),
        Action::HoldPending { review_on, legal_reference } => json!({ "hold_pending": { "review_on": review_on, "legal_ref": legal_reference } }),
        Action::WriteOff { amount_minor } => json!({ "write_off_held": { "amount_minor": amount_minor } }),
        Action::Split { shares } => json!({ "split": { "shares": shares.iter()
            .map(|s| json!({ "party": s.party, "amount_minor": s.amount_minor, "agreement_ref": s.agreement })).collect::<Vec<_>>() } }),
        Action::RecordDividend { receivable, received_minor } => {
            json!({ "record_dividend": { "debt": debt(receivable), "received_minor": received_minor, "receipt_ref": document } })
        }
    }
}

/// The commission module takes action IDs of letters, digits and hyphens.
fn their_id(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// The envelopes that replay one approved action in the commission module:
/// the maker's proposal, then each approver's own signature, each sent as
/// the person who did it. That module checks every one against its own
/// policy and limits; nothing here approves on anyone's behalf.
pub fn envelopes_for(proposal: &Proposal, people: &Value) -> Vec<Value> {
    let id = their_id(&proposal.id);
    let mut out = vec![json!({
        "key": format!("workbench:{id}:propose"),
        "actor": { "staff": people[proposal.maker.to_string()] },
        "command": { "propose_action": {
            "id": id, "party": proposal.party, "action": their_action(&proposal.action, &proposal.document),
            "reason_code": proposal.reason_code, "document_ref": proposal.document,
        } },
    })];
    for approval in &proposal.approvals {
        let role = match (approval.legal, approval.conduct) {
            (true, _) => "legal",
            (_, true) => "conduct_officer",
            _ => "counterparty_finance_approver",
        };
        out.push(json!({
            "key": format!("workbench:{id}:approve:{}", approval.by),
            "actor": { "staff": people[approval.by.to_string()] },
            "command": { "approve_action": { "id": id, "as_role": role } },
        }));
    }
    out
}

/// The role an approval envelope signs in.
fn envelope_role(envelope: &Value) -> Option<&str> {
    envelope["command"]["approve_action"]["as_role"].as_str()
}

/// Why the commission module refused a command, if it did.
fn refusal(output: &Value) -> Option<(String, String)> {
    let err = output["result"].get("Err")?;
    Some((
        err["code"].as_str().unwrap_or("refused").to_string(),
        err["message"].as_str().unwrap_or("the commission module refused it").to_string(),
    ))
}

/// Carry every action approved on the workbench to the commission module
/// and bring back what became of it. Returns how many were closed.
pub async fn carry_actions(source: &dyn Source, channel: &dyn Channel, at: At) -> Result<usize, String> {
    let question = Question::WorkbenchApproved {
        module: PUBLISHER.to_string(),
        at,
    };
    let waiting = channel.ask(question).await.map_err(|e| e.0)?;
    let mut closed = 0;
    for entry in waiting.as_array().into_iter().flatten() {
        let proposal: Proposal = serde_json::from_value(entry["proposal"].clone()).map_err(|e| format!("an approved action was not understood: {e}"))?;
        let id = their_id(&proposal.id);
        // Each envelope has its own key, so an action half sent before a restart carries on where it stopped.
        let mut refused = None;
        for envelope in envelopes_for(&proposal, &entry["people"]) {
            let sent = envelope.clone();
            let output = source.tell(envelope).await?;
            // A repeat of something already done answers with the first result, so a refusal here is a real one.
            if let Some(why) = refusal(&output) {
                // The workbench asks for the conduct officer whenever a conduct case is open; that module asks
                // only where its own rules do. A signature it did not need is not a failure of the action.
                if why.0 == "approval_not_needed" && envelope_role(&sent) == Some("conduct_officer") {
                    continue;
                }
                refused = Some(why);
                break;
            }
        }
        let record = source.ask(json!({ "action": { "id": id } })).await?;
        let state = record["state"].as_str().unwrap_or_default();
        let outcome = match (refused, state) {
            (_, "completed") => Some((Some(format!("distribution:{id}")), None)),
            (_, "posting") => None,
            (Some(why), _) => Some((None, Some(why))),
            (None, "rejected") => Some((
                None,
                Some((
                    "rejected".to_string(),
                    record["rejected_because"].as_str().unwrap_or("rejected in the commission module").to_string(),
                )),
            )),
            (None, _) => match record["failed"].as_str() {
                Some(why) => Some((None, Some(("failed".to_string(), why.to_string())))),
                // Every signature given here was accepted and that module still wants more: its thresholds are
                // stricter than the workbench's. Say so; the action stays open there for the people it names.
                None => Some((
                    None,
                    Some((
                        "needs_more_approval".to_string(),
                        format!("the commission module still needs: {}", record["needed"]),
                    )),
                )),
            },
        };
        let Some((reference, failed)) = outcome else { continue };
        let envelope = Envelope {
            key: format!("workbench:{}:resolve", proposal.id),
            at,
            actor: Actor::Module(PUBLISHER.to_string()),
            command: Command::Workbench(WorkbenchCommand::Resolve {
                id: proposal.id.clone(),
                reference,
                failed,
            }),
        };
        let output = channel.propose(envelope).await.map_err(|e| e.0)?;
        output
            .result
            .map_err(|refusal| format!("the channel module refused the outcome of {}: {refusal:?}", proposal.id))?;
        closed += 1;
    }
    Ok(closed)
}

/// Forward every `every`, for as long as the service runs. A pass that
/// fails is tried again from the same cursor.
pub async fn forward(source: Arc<dyn Source>, channel: Arc<dyn Channel>, every: Duration, clock: Arc<dyn Fn() -> At + Send + Sync>, currency: String) {
    let mut ticker = tokio::time::interval(every);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut cursor = 0;
    loop {
        ticker.tick().await;
        match pass(source.as_ref(), channel.as_ref(), cursor, clock(), &currency).await {
            Ok(next) => cursor = next,
            Err(reason) => tracing::warn!(%reason, cursor, "nothing forwarded from the commission module this pass"),
        }
        if let Err(reason) = carry_actions(source.as_ref(), channel.as_ref(), clock()).await {
            tracing::warn!(%reason, "approved workbench actions were not carried to the commission module this pass");
        }
    }
}
