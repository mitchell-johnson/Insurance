//! A bank for tests, in this process. It speaks the wire form [`HttpBank`]
//! speaks, checks what a bank would check, remembers what it was sent, and
//! can be told to fail in the ways banks and networks fail.
//!
//! [`HttpBank`]: crate::bank::HttpBank

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use chrono::NaiveDate;
use ic_payments::file::{BankMessage, Batch, Manifest};
use ic_payments::iso20022;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::bank::{Document, Numbered};

/// What the bank holds under one ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub sha256: String,
    pub reference: String,
    /// Item IDs with their amounts, as the bank read them.
    pub items: Vec<(String, i64)>,
}

#[derive(Default)]
pub struct Inner {
    /// Bearer tokens the bank accepts: one per whitelisted region.
    pub tokens: BTreeSet<String>,
    pub held: BTreeMap<String, Held>,
    pub messages: Vec<BankMessage>,
    /// Refuse the next submission with this reason.
    pub refuse_next: Option<String>,
    /// Take the next submission and answer with an error: the sender cannot
    /// tell that it arrived.
    pub swallow_next: bool,
    /// Lose the next submission and answer with an error.
    pub drop_next: bool,
    /// Answer everything with an error.
    pub down: bool,
    /// Items to refuse at acknowledgement, with the reason.
    pub reject_items: BTreeMap<String, String>,
    /// Submissions that reached the bank, repeats included.
    pub arrivals: u64,
    /// The bank speaks ISO 20022 instead of this system's own format.
    pub iso: bool,
    /// The account number the bank writes for each of the module's account
    /// codes, in an ISO 20022 statement.
    pub account_numbers: BTreeMap<String, String>,
    /// Documents sent as they are, by their place in the sequence, in place
    /// of the message there: what a bank writes that the module's own
    /// messages cannot say.
    pub documents: BTreeMap<u64, String>,
}

#[derive(Clone)]
pub struct FakeBank {
    pub url: String,
    inner: Arc<Mutex<Inner>>,
}

type Answer = (StatusCode, Json<Value>);

fn say(status: StatusCode, body: Value) -> Answer {
    (status, Json(body))
}

fn admit(inner: &Inner, headers: &HeaderMap) -> Result<(), Answer> {
    if inner.down {
        return Err(say(StatusCode::SERVICE_UNAVAILABLE, json!("the bank is down")));
    }
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if token.is_some_and(|t| inner.tokens.contains(t)) {
        Ok(())
    } else {
        Err(say(StatusCode::FORBIDDEN, json!("this address is not on the bank's allow list")))
    }
}

/// Take a submission the bank has parsed and checked.
fn take(inner: &mut Inner, id: &str, sha256: String, items: Vec<(String, i64)>) -> Result<Held, Answer> {
    inner.arrivals += 1;
    if let Some(reason) = inner.refuse_next.take() {
        return Err(say(StatusCode::UNPROCESSABLE_ENTITY, json!(reason)));
    }
    if std::mem::take(&mut inner.drop_next) {
        return Err(say(StatusCode::BAD_GATEWAY, json!("connection reset")));
    }
    let held = match inner.held.get(id) {
        Some(held) if held.sha256 == sha256 => held.clone(),
        Some(_) => return Err(say(StatusCode::CONFLICT, json!("another file was sent under this ID"))),
        None => {
            let held = Held {
                sha256,
                reference: format!("BANK-{}", inner.held.len() + 1),
                items,
            };
            inner.held.insert(id.to_string(), held.clone());
            held
        }
    };
    if std::mem::take(&mut inner.swallow_next) {
        return Err(say(StatusCode::GATEWAY_TIMEOUT, json!("timed out")));
    }
    Ok(held)
}

fn acknowledgement(inner: &Inner, id: &str, held: &Held) -> BankMessage {
    BankMessage::Acknowledgement {
        id: id.to_string(),
        refused: None,
        rejected: held
            .items
            .iter()
            .filter_map(|(item, _)| inner.reject_items.get(item).map(|r| (item.clone(), r.clone())))
            .collect(),
    }
}

async fn put_file(State(bank): State<FakeBank>, Path(id): Path<String>, headers: HeaderMap, body: String) -> Answer {
    let mut inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let bad = |why: &str| say(StatusCode::UNPROCESSABLE_ENTITY, json!(why));
    let manifest = headers.get("x-manifest").and_then(|h| base64::engine::general_purpose::STANDARD.decode(h).ok());
    let Some(manifest) = manifest.and_then(|m| serde_json::from_slice::<Manifest>(&m).ok()) else {
        return bad("no manifest");
    };
    let sha256 = hex::encode(Sha256::digest(body.as_bytes()));
    if manifest.file_id != id || manifest.sha256 != sha256 || manifest.signature.is_empty() {
        return bad("the manifest does not match the file");
    }
    let items = if inner.iso {
        match iso_items(&body, &id) {
            Ok(items) => items,
            Err(why) => return bad(&why),
        }
    } else {
        // The trailer's count and total are checked before a line is touched.
        let mut items = Vec::new();
        let mut trailer = None;
        for line in body.lines() {
            let fields: Vec<&str> = line.split('|').collect();
            match fields.as_slice() {
                ["D", item, _, _, amount] => items.push((item.to_string(), amount.parse::<i64>().unwrap_or(0))),
                ["T", count, total] => trailer = Some((count.parse::<usize>().ok(), total.parse::<i64>().ok())),
                _ => {}
            }
        }
        if trailer != Some((Some(items.len()), Some(items.iter().map(|(_, a)| a).sum()))) {
            return bad("the trailer does not match the lines");
        }
        items
    };
    match take(&mut inner, &id, sha256, items) {
        Ok(held) => say(StatusCode::CREATED, json!({"reference": held.reference})),
        Err(no) => no,
    }
}

/// An ISO 20022 initiation's items, once its header agrees with its lines
/// and its message ID is the one it was sent under.
fn iso_items(body: &str, id: &str) -> Result<Vec<(String, i64)>, String> {
    let initiation = iso20022::read_initiation(body)?;
    if initiation.message != id {
        return Err("the message ID is not the ID it was sent under".to_string());
    }
    if initiation.declared != (initiation.items.len(), initiation.items.iter().map(|(_, a)| a).sum()) {
        return Err("the count and control sum do not match the transactions".to_string());
    }
    Ok(initiation.items)
}

async fn post_batch(State(bank): State<FakeBank>, headers: HeaderMap, body: String) -> Answer {
    let mut inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let key = headers.get("idempotency-key").and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
    let read = if inner.iso {
        iso_items(&body, &key).map(|items| (key.clone(), hex::encode(Sha256::digest(body.as_bytes())), items))
    } else {
        match serde_json::from_str::<Batch>(&body) {
            Ok(batch) if batch.id == key => {
                let items = batch.items.iter().map(|i| (i.item.clone(), i.amount_minor)).collect();
                batch.content_hash().map(|hash| (batch.id, hash, items))
            }
            Ok(_) => return say(StatusCode::BAD_REQUEST, json!("the idempotency key is the batch's ID")),
            Err(e) => Err(e.to_string()),
        }
    };
    let (id, sha256, items) = match read {
        Ok(read) => read,
        Err(why) => return say(StatusCode::UNPROCESSABLE_ENTITY, json!(why)),
    };
    match take(&mut inner, &id, sha256, items) {
        // An API answers for every item at once.
        Ok(held) => say(
            StatusCode::OK,
            json!({"reference": held.reference, "acknowledgement": acknowledgement(&inner, &id, &held)}),
        ),
        Err(no) => no,
    }
}

async fn get_held(State(bank): State<FakeBank>, Path(id): Path<String>, headers: HeaderMap) -> Answer {
    let inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    match inner.held.get(&id) {
        Some(held) => say(StatusCode::OK, json!({"sha256": held.sha256})),
        None => say(StatusCode::NOT_FOUND, json!("not held")),
    }
}

#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: u64,
}

async fn get_messages(State(bank): State<FakeBank>, Query(q): Query<After>, headers: HeaderMap) -> Answer {
    let inner = bank.lock();
    if let Err(no) = admit(&inner, &headers) {
        return no;
    }
    let numbered = inner.messages.iter().zip(1..).filter(|(_, seq)| *seq > q.after);
    if inner.iso {
        let documents: Vec<Document> = numbered
            .map(|(m, seq)| Document {
                seq,
                document: inner.documents.get(&seq).cloned().unwrap_or_else(|| iso_document(m, &inner.account_numbers)),
            })
            .collect();
        return say(StatusCode::OK, json!(documents));
    }
    let messages: Vec<Numbered> = numbered
        .map(|(m, seq)| Numbered {
            seq,
            part: 0,
            message: m.clone(),
        })
        .collect();
    say(StatusCode::OK, json!(messages))
}

/// Connections the bank takes only once the TLS handshake, with a client
/// certificate it trusts, has succeeded. A caller that fails it is dropped,
/// as a bank's front end drops it.
struct TlsListener {
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
}

impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = std::net::SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((stream, addr)) = self.tcp.accept().await else { continue };
            if let Ok(tls) = self.acceptor.accept(stream).await {
                return (tls, addr);
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

/// The certificates a bank speaking mutual TLS holds, in PEM: its own
/// certificate and key, and the authority whose client certificates it
/// accepts.
pub struct BankTls<'a> {
    pub certificate: &'a str,
    pub key: &'a str,
    pub client_authority: &'a str,
}

fn invalid(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())
}

impl BankTls<'_> {
    fn acceptor(&self) -> std::io::Result<tokio_rustls::TlsAcceptor> {
        use rustls::pki_types::pem::PemObject;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
        let mut roots = rustls::RootCertStore::empty();
        for cert in CertificateDer::pem_slice_iter(self.client_authority.as_bytes()) {
            roots.add(cert.map_err(invalid)?).map_err(invalid)?;
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(invalid)?;
        let chain = CertificateDer::pem_slice_iter(self.certificate.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        let key = PrivateKeyDer::from_pem_slice(self.key.as_bytes()).map_err(invalid)?;
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .with_client_cert_verifier(verifier)
            .with_single_cert(chain, key)
            .map_err(invalid)?;
        Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
    }
}

impl FakeBank {
    /// Start a bank on a free local port. It accepts the tokens given.
    pub async fn start(tokens: &[&str]) -> std::io::Result<Self> {
        Self::serve(tokens, None).await
    }

    /// Start a bank that speaks HTTPS and takes only callers presenting a
    /// client certificate from the authority given.
    pub async fn start_tls(tokens: &[&str], tls: BankTls<'_>) -> std::io::Result<Self> {
        Self::serve(tokens, Some(tls.acceptor()?)).await
    }

    async fn serve(tokens: &[&str], tls: Option<tokio_rustls::TlsAcceptor>) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let scheme = if tls.is_some() { "https" } else { "http" };
        let bank = Self {
            url: format!("{scheme}://{}", listener.local_addr()?),
            inner: Arc::new(Mutex::new(Inner {
                tokens: tokens.iter().map(|t| t.to_string()).collect(),
                ..Inner::default()
            })),
        };
        let app = Router::new()
            .route("/files/{id}", put(put_file).get(get_held))
            .route("/batches", post(post_batch))
            .route("/batches/{id}", get(get_held))
            .route("/messages", get(get_messages))
            .with_state(bank.clone());
        match tls {
            None => {
                tokio::spawn(async move {
                    let _ = axum::serve(listener, app).await;
                });
            }
            Some(acceptor) => {
                let listener = TlsListener { tcp: listener, acceptor };
                tokio::spawn(async move {
                    let _ = axum::serve(listener, app).await;
                });
            }
        }
        Ok(bank)
    }

    /// The bank's books, to read or to arrange a failure.
    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("the fake bank's books")
    }

    /// Send the acknowledgement file for something the bank holds.
    pub fn acknowledge(&self, id: &str) {
        let mut inner = self.lock();
        if let Some(held) = inner.held.get(id).cloned() {
            let message = acknowledgement(&inner, id, &held);
            inner.messages.push(message);
        }
    }

    pub fn settle(&self, id: &str, settled_on: NaiveDate) {
        self.push(BankMessage::Settlement {
            id: id.to_string(),
            settled_on,
        });
    }

    pub fn push(&self, message: BankMessage) {
        self.lock().messages.push(message);
    }

    /// Send an ISO 20022 document exactly as written.
    pub fn push_document(&self, document: &str) {
        let mut inner = self.lock();
        inner.messages.push(BankMessage::Settlement {
            id: String::new(),
            settled_on: NaiveDate::MIN,
        });
        let seq = inner.messages.len() as u64;
        inner.documents.insert(seq, document.to_string());
    }
}

fn iso_amount(minor: i64) -> String {
    let indicator = if minor < 0 { "DBIT" } else { "CRDT" };
    let minor = minor.abs();
    format!("<Amt Ccy=\"NZD\">{}.{:02}</Amt><CdtDbtInd>{indicator}</CdtDbtInd>", minor / 100, minor % 100)
}

fn iso_wrap(kind: &str, body: String) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Document xmlns=\"urn:iso:std:iso:20022:tech:xsd:{kind}\">{body}</Document>\n")
}

/// A message as a bank that speaks ISO 20022 would send it: an
/// acknowledgement as a `pain.002`, a settlement or a return as a
/// `camt.054` notification on the account it touched, a statement as a
/// `camt.053`. Settlements and returns name the account `settlement`, which
/// a test maps to an account code.
pub fn iso_document(message: &BankMessage, numbers: &BTreeMap<String, String>) -> String {
    let number = |code: &str| numbers.get(code).cloned().unwrap_or_else(|| code.to_string());
    let notification = |entry: String| {
        iso_wrap(
            "camt.054.001.08",
            format!(
                "<BkToCstmrDbtCdtNtfctn><GrpHdr><MsgId>N</MsgId></GrpHdr><Ntfctn><Acct><Id><Othr><Id>{}</Id></Othr></Id></Acct>{entry}</Ntfctn></BkToCstmrDbtCdtNtfctn>",
                number("settlement")
            ),
        )
    };
    match message {
        BankMessage::Acknowledgement { id, refused, rejected } => {
            let group = match refused {
                Some(reason) => format!("<GrpSts>RJCT</GrpSts><StsRsnInf><Rsn><Cd>NARR</Cd></Rsn><AddtlInf>{reason}</AddtlInf></StsRsnInf>"),
                None => "<GrpSts>ACCP</GrpSts>".to_string(),
            };
            let txs: String = rejected
                .iter()
                .map(|(item, reason)| {
                    format!("<TxInfAndSts><OrgnlEndToEndId>{item}</OrgnlEndToEndId><TxSts>RJCT</TxSts><StsRsnInf><Rsn><Cd>{reason}</Cd></Rsn></StsRsnInf></TxInfAndSts>")
                })
                .collect();
            iso_wrap(
                "pain.002.001.10",
                format!("<CstmrPmtStsRpt><OrgnlGrpInfAndSts><OrgnlMsgId>{id}</OrgnlMsgId>{group}</OrgnlGrpInfAndSts><OrgnlPmtInfAndSts>{txs}</OrgnlPmtInfAndSts></CstmrPmtStsRpt>"),
            )
        }
        BankMessage::Settlement { id, settled_on } => notification(format!(
            "<Ntry>{}<BookgDt><Dt>{settled_on}</Dt></BookgDt><NtryDtls><Btch><PmtInfId>{id}</PmtInfId></Btch></NtryDtls></Ntry>",
            iso_amount(0)
        )),
        BankMessage::Return { item, reason, returned_on } => notification(format!(
            "<Ntry>{}<BookgDt><Dt>{returned_on}</Dt></BookgDt><NtryDtls><TxDtls><Refs><EndToEndId>{item}</EndToEndId></Refs><RtrInf><Rsn><Cd>{}</Cd></Rsn></RtrInf></TxDtls></NtryDtls></Ntry>",
            iso_amount(0),
            iso20022::reason_code(reason)
        )),
        BankMessage::Statement {
            account,
            date,
            opening_minor,
            closing_minor,
            lines,
        } => {
            let balance = |code: &str, minor: i64| format!("<Bal><Tp><CdOrPrtry><Cd>{code}</Cd></CdOrPrtry></Tp>{}<Dt><Dt>{date}</Dt></Dt></Bal>", iso_amount(minor));
            let entries: String = lines
                .iter()
                .map(|l| {
                    let payer = l
                        .payer_account
                        .as_ref()
                        .map(|p| format!("<RltdPties><DbtrAcct><Id><Othr><Id>{p}</Id></Othr></Id></DbtrAcct></RltdPties>"))
                        .unwrap_or_default();
                    format!(
                        "<Ntry><NtryRef>{}</NtryRef>{}<BookgDt><Dt>{date}</Dt></BookgDt><NtryDtls><TxDtls>{payer}<RmtInf><Ustrd>{}</Ustrd></RmtInf></TxDtls></NtryDtls></Ntry>",
                        l.id,
                        iso_amount(l.amount_minor),
                        l.reference
                    )
                })
                .collect();
            iso_wrap(
                "camt.053.001.08",
                format!(
                    "<BkToCstmrStmt><GrpHdr><MsgId>S</MsgId></GrpHdr><Stmt><Acct><Id><Othr><Id>{}</Id></Othr></Id></Acct>{}{}{entries}</Stmt></BkToCstmrStmt>",
                    number(account),
                    balance("OPBD", *opening_minor),
                    balance("CLBD", *closing_minor)
                ),
            )
        }
    }
}
