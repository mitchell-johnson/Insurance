//! The pump: one pass over everything the module is waiting on.
//!
//! A pass asks the bank about submissions whose fate is unknown, sends what
//! is queued, brings the bank's messages in, and works the outbox. Each
//! step is safe to repeat and safe to interrupt. Any number of pumps may
//! run, in any region: the module lets one of them take each submission.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::{Duration, NaiveDateTime};
use ic_azure::blob::WormStore;
use ic_azure::AzureError;
use ic_cashmatch::IncomingPayment;
use ic_money::Money;
use ic_payments::config::{Config, WireFormat};
use ic_payments::file::{BankMessage, Batch, BatchItem, Direction};
use ic_payments::model::{Instruction, OutboxEntry, Probe, Purpose, Role, Submission};
use ic_payments::{Actor, Command, Envelope, Reject};
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::bank::{Bank, ChannelError, Target};
use crate::ports::{Core, Keys, Module, Notifier, Secrets};

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// Submissions the bank now holds.
    pub sent: usize,
    /// Submissions that failed and need a person to send what is unpaid again.
    pub failed: usize,
    /// Messages from banks recorded in the module.
    pub messages: usize,
    /// Outbox entries finished.
    pub instructions: usize,
    /// What could not be done this time. The next pass tries again.
    pub problems: Vec<String>,
}

/// The wall clock in New Zealand, which is what cutoffs are measured in.
pub fn nz_now() -> NaiveDateTime {
    chrono::Utc::now().with_timezone(&chrono_tz::Pacific::Auckland).naive_local()
}

pub struct Pump<M, C, B, V, K, S, N> {
    pub module: M,
    pub core: C,
    pub bank: B,
    /// The evidence vault: every file and manifest sent, write once.
    pub vault: V,
    pub keys: K,
    pub secrets: S,
    pub notifier: N,
    /// The region this pump runs in. It uses that region's endpoints.
    pub region: String,
    /// Unique to this process, so two pumps never share a claim on a submission.
    pub instance: String,
    pub clock: Arc<dyn Fn() -> NaiveDateTime + Send + Sync>,
    /// How long a submission may sit with a driver before the bank is asked
    /// what became of it. Far longer than any request can take.
    pub grace: Duration,
    /// The last message read from each channel.
    pub cursors: Mutex<BTreeMap<String, u64>>,
}

impl<M: Module, C: Core, B: Bank, V: WormStore, K: Keys, S: Secrets, N: Notifier> Pump<M, C, B, V, K, S, N> {
    async fn tell(&self, key: String, command: Command) -> Result<(), String> {
        let envelope = Envelope {
            key,
            actor: Actor {
                id: format!("payments-pump-{}", self.instance),
                roles: [Role::Service].into(),
            },
            at: (self.clock)(),
            command,
        };
        match self.module.propose(envelope).await? {
            Ok(_) => Ok(()),
            Err(Reject { code, message }) => Err(format!("{code}: {message}")),
        }
    }

    async fn ask<T: DeserializeOwned>(&self, question: serde_json::Value) -> Result<T, String> {
        serde_json::from_value(self.module.query(question).await?).map_err(crate::ports::unreadable)
    }

    fn target(&self, config: &Config, channel: &str) -> Result<Target, String> {
        let ch = config.channels.get(channel).ok_or_else(|| format!("channel {channel} is not configured"))?;
        let endpoint = ch
            .endpoints
            .get(&self.region)
            .ok_or_else(|| format!("channel {channel} has no endpoint in {}", self.region))?;
        let token = self
            .secrets
            .secret(&endpoint.credential)
            .ok_or_else(|| format!("the credential {} is not available", endpoint.credential))?;
        let accounts = config
            .accounts
            .iter()
            .filter(|(_, a)| a.bank == ch.bank)
            .filter_map(|(code, a)| Some((a.at_bank.as_ref()?.number.clone(), code.clone())))
            .collect();
        let secret = |name: &Option<String>, what: &str| match name {
            None => Ok(None),
            Some(name) => self.secrets.secret(name).map(Some).ok_or_else(|| format!("the {what} {name} is not available")),
        };
        Ok(Target {
            identity: secret(&endpoint.client_certificate, "client certificate")?,
            authority: secret(&endpoint.bank_authority, "bank's certificate authority")?,
            accounts,
            kind: ch.kind,
            format: ch.format,
            url: endpoint.url.trim_end_matches('/').to_string(),
            token,
        })
    }

    /// Sign a batch, put the file and its manifest in the vault, and send it.
    async fn dispatch(&self, config: &Config, to: &Target, batch: &Batch) -> Result<crate::bank::Receipt, ChannelError> {
        let before = |e: String| ChannelError::NotSent(e);
        let key = config
            .channels
            .get(&batch.channel)
            .and_then(|c| c.endpoints.get(&self.region))
            .map(|e| e.signing_key.clone())
            .unwrap_or_default();
        let file = batch.wire().map_err(before)?;
        let signature = self.keys.sign(&key, file.as_bytes()).await.map_err(before)?;
        let manifest = batch.manifest(signature.algorithm, signature.key_id, signature.value).map_err(before)?;
        let manifest_bytes = serde_json::json!(manifest).to_string().into_bytes();
        // Evidence first: nothing goes to a bank that the vault does not
        // hold. The same batch written twice is the same evidence.
        let (file_name, file_kind) = match batch.format {
            WireFormat::Own => ("file.txt", "text/plain"),
            WireFormat::Iso20022 => ("file.xml", "application/xml"),
        };
        for (name, kind, bytes) in [
            (format!("payments/{}/{file_name}", batch.id), file_kind, file.into_bytes()),
            (format!("payments/{}/manifest.json", batch.id), "application/json", manifest_bytes),
        ] {
            match self.vault.put_new(&name, kind, bytes).await {
                Ok(()) | Err(AzureError::AlreadyExists(_)) => {}
                Err(e) => return Err(before(format!("the evidence vault refused {name}: {e}"))),
            }
        }
        self.bank.submit(to, batch, &manifest).await
    }

    /// Ask the bank what became of submissions a driver took and never
    /// reported on.
    async fn resolve_unconfirmed(&self, config: &Config, pass: &mut Pass) -> Result<(), String> {
        let unsure: Vec<(String, Submission)> = self.ask(json!({"q": "unconfirmed_submissions"})).await?;
        let now = (self.clock)();
        for (id, submission) in unsure {
            if submission.begun_at.is_some_and(|begun| now - begun < self.grace) {
                continue;
            }
            let outcome = match self.target(config, &submission.channel) {
                Ok(to) => self.bank.holds(&to, &id).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let recorded = match outcome {
                Ok(Some(content_hash)) => {
                    pass.sent += 1;
                    let sent = Command::RecordSubmitted {
                        submission: id.clone(),
                        content_hash,
                        bank_reference: None,
                    };
                    self.tell(format!("sent-{id}"), sent).await
                }
                Ok(None) => {
                    pass.failed += 1;
                    let lost = Command::RecordNotReceived {
                        submission: id.clone(),
                        reason: "the bank does not hold it".to_string(),
                    };
                    self.tell(format!("lost-{id}"), lost).await
                }
                Err(e) => Err(e),
            };
            if let Err(e) = recorded {
                pass.problems.push(format!("submission {id}: {e}"));
            }
        }
        Ok(())
    }

    async fn send_queued(&self, config: &Config, pass: &mut Pass) -> Result<(), String> {
        let queued: Vec<(String, Submission)> = self.ask(json!({"q": "queued_submissions"})).await?;
        for (id, submission) in queued {
            if let Err(e) = self.send_one(config, &id, &submission, pass).await {
                pass.problems.push(format!("submission {id}: {e}"));
            }
        }
        Ok(())
    }

    async fn send_one(&self, config: &Config, id: &str, submission: &Submission, pass: &mut Pass) -> Result<(), String> {
        let to = self.target(config, &submission.channel)?;
        // While the bank cannot be reached the submission stays queued, to
        // go later or by another route. Nothing is taken that cannot be sent.
        let already = self.bank.holds(&to, id).await.map_err(|e| e.to_string())?;
        let begin = Command::BeginSubmission {
            submission: id.to_string(),
            region: self.region.clone(),
        };
        self.tell(format!("begin-{id}-{}", self.instance), begin).await?;
        let batch: Batch = self
            .ask::<Option<Batch>>(json!({"q": "batch", "submission": id}))
            .await?
            .ok_or("the submission has no batch")?;
        let sent = |content_hash: String, bank_reference: Option<String>| Command::RecordSubmitted {
            submission: id.to_string(),
            content_hash,
            bank_reference,
        };
        if let Some(content_hash) = already {
            pass.sent += 1;
            return self.tell(format!("sent-{id}"), sent(content_hash, None)).await;
        }
        match self.dispatch(config, &to, &batch).await {
            Ok(receipt) => {
                pass.sent += 1;
                self.tell(format!("sent-{id}"), sent(manifest_hash(&batch), Some(receipt.reference))).await?;
                if let Some(acknowledgement) = receipt.acknowledgement {
                    self.tell(format!("ack-{id}"), acknowledgement.command()).await?;
                }
                Ok(())
            }
            Err(ChannelError::Refused(reason)) => {
                pass.failed += 1;
                let refused = Command::RecordAcknowledgement {
                    submission: id.to_string(),
                    refused: Some(reason),
                    rejected: BTreeMap::new(),
                };
                self.tell(format!("refused-{id}"), refused).await
            }
            Err(ChannelError::NotSent(reason)) => {
                pass.failed += 1;
                let lost = Command::RecordNotReceived {
                    submission: id.to_string(),
                    reason,
                };
                self.tell(format!("lost-{id}"), lost).await
            }
            // The bank may hold it. It stays with this driver until the
            // grace period has passed and the bank can be asked.
            Err(ChannelError::Unknown(reason)) => Err(format!("no clear answer from the bank, it will be asked: {reason}")),
        }
    }

    async fn collect_messages(&self, config: &Config, pass: &mut Pass) {
        // What any driver has already recorded is not read again, so a
        // restart does not walk a bank's whole history.
        let recorded: BTreeMap<String, u64> = self.ask(json!({"q": "messages_read"})).await.unwrap_or_default();
        for channel in config.channels.keys() {
            let held = self.cursors.lock().map(|c| c.get(channel).copied().unwrap_or(0)).unwrap_or(0);
            let after = held.max(recorded.get(channel).copied().unwrap_or(0));
            // The module is told how far only up to the first message it
            // did not take, so a restart offers that message again.
            let (mut through, mut unbroken) = (after, true);
            let messages = match self.target(config, channel) {
                Ok(to) => self.bank.messages(&to, after).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let messages = match messages {
                Ok(messages) => messages,
                Err(e) => {
                    pass.problems.push(format!("channel {channel}: {e}"));
                    continue;
                }
            };
            for numbered in messages {
                // The bank's answers to a drill's token batch belong to no run.
                let about_drill =
                    matches!(&numbered.message, BankMessage::Acknowledgement { id, .. } | BankMessage::Settlement { id, .. } if id.starts_with("drill-"));
                if !about_drill {
                    let key = match numbered.part {
                        0 => format!("bank-{channel}-{}", numbered.seq),
                        part => format!("bank-{channel}-{}-{part}", numbered.seq),
                    };
                    match self.tell(key, numbered.message.command()).await {
                        Ok(()) => pass.messages += 1,
                        // The bank said something the module will not take.
                        // That is a break between us and the bank: tell
                        // someone, and do not read it again.
                        Err(e) => {
                            // The rest of a document whose earlier parts
                            // were taken is offered again with them.
                            if unbroken {
                                through = through.min(numbered.seq - 1);
                            }
                            unbroken = false;
                            let detail = format!("channel {channel} message {}: {e}", numbered.seq);
                            if let Err(n) = self.notifier.notify(Role::Reconciler, "bank_message_refused", &detail).await {
                                pass.problems.push(n);
                            }
                            pass.problems.push(detail);
                        }
                    }
                }
                if unbroken {
                    through = numbered.seq;
                }
                if let Ok(mut cursors) = self.cursors.lock() {
                    cursors.insert(channel.clone(), numbered.seq);
                }
            }
            if through > recorded.get(channel).copied().unwrap_or(0) {
                let note = Command::NoteMessagesRead {
                    channel: channel.clone(),
                    through,
                };
                pass.problems.extend(self.tell(format!("read-{channel}-{through}"), note).await.err());
            }
        }
    }

    async fn work_outbox(&self, pass: &mut Pass) -> Result<(), String> {
        let outbox: BTreeMap<u64, OutboxEntry> = self.ask(json!({"q": "outbox"})).await?;
        for (entry, work) in outbox {
            let outcome = match work.instruction {
                Instruction::Core { caller, command } => self.core.send(&work.key, &caller, *command).await,
                Instruction::Module { module, command } => self.module.tell(&module, command).await.map(|answer| crate::ports::refusal(&answer)),
                Instruction::CoreAt { policy, caller, command } => self.core.send_to(&policy, &work.key, &caller, *command).await,
                Instruction::Match {
                    amount_minor,
                    currency,
                    reference,
                    payer_account,
                    received_on,
                    ..
                } => {
                    let payment = IncomingPayment {
                        id: work.key.clone(),
                        amount: Money::new(amount_minor, currency),
                        reference: Some(reference),
                        payer_account,
                        received_on,
                    };
                    self.core.match_receipt(payment).await
                }
                Instruction::Notify { to, subject, detail } => self.notifier.notify(to, &subject, &detail).await.map(|()| None),
            };
            match outcome {
                Ok(refused) => {
                    self.tell(format!("done-{}", work.key), Command::CompleteInstruction { entry, refused }).await?;
                    pass.instructions += 1;
                }
                // The outbox is in order, and order matters to the core: a
                // return must not overtake its settlement. Stop here.
                Err(e) => {
                    pass.problems.push(format!("outbox {}: {e}", work.key));
                    break;
                }
            }
        }
        Ok(())
    }

    /// One pass. Never fails: what could not be done is in the report.
    pub async fn pass(&self) -> Pass {
        let mut pass = Pass::default();
        let config = match self.ask::<Option<Config>>(json!({"q": "config"})).await {
            Ok(Some(config)) => config,
            Ok(None) => {
                pass.problems.push("the module has no configuration".to_string());
                return pass;
            }
            Err(e) => {
                pass.problems.push(format!("the module could not be asked: {e}"));
                return pass;
            }
        };
        if let Err(e) = self.resolve_unconfirmed(&config, &mut pass).await {
            pass.problems.push(e);
        }
        if let Err(e) = self.send_queued(&config, &mut pass).await {
            pass.problems.push(e);
        }
        self.collect_messages(&config, &mut pass).await;
        if let Err(e) = self.work_outbox(&mut pass).await {
            pass.problems.push(e);
        }
        pass
    }

    /// The evacuation drill from this pump's region: a token payment through
    /// every channel, a file where the channel takes files and an API
    /// request where it takes those, each signed, kept in the vault and sent
    /// exactly as a real one is. Returns whether every channel was reached.
    pub async fn drill(&self, id: &str) -> Result<bool, String> {
        let config: Config = self
            .ask::<Option<Config>>(json!({"q": "config"}))
            .await?
            .ok_or("the module has no configuration")?;
        let start = Command::StartDrill {
            drill: id.to_string(),
            region: self.region.clone(),
        };
        self.tell(format!("drill-{id}-start"), start).await?;
        let mut reached = true;
        for (channel, ch) in &config.channels {
            let mut batch = Batch {
                id: format!("drill-{id}-{channel}"),
                channel: channel.clone(),
                account: config
                    .accounts
                    .iter()
                    .find(|(_, a)| a.bank == ch.bank && a.purpose == Purpose::Operating)
                    .map(|(code, _)| code.clone())
                    .unwrap_or_default(),
                direction: Direction::Credit,
                value_date: (self.clock)().date(),
                currency: ic_money::Currency::NZD,
                format: ch.format,
                ordering: None,
                items: vec![BatchItem {
                    item: format!("drill-{id}-{channel}-1"),
                    name: config.rules.drill_account.name.clone(),
                    number: config.rules.drill_account.number.clone(),
                    amount_minor: config.rules.drill_amount_minor,
                }],
            };
            batch.ordering = config.wire(channel, &batch.account).1;
            let outcome = match self.target(&config, channel) {
                Ok(to) => self.dispatch(&config, &to, &batch).await.map_err(|e| e.to_string()),
                Err(e) => Err(e),
            };
            let probe = Probe {
                ok: outcome.is_ok(),
                evidence_hash: manifest_hash(&batch),
                detail: match outcome {
                    Ok(receipt) => format!("the bank took {} as {}", batch.id, receipt.reference),
                    Err(e) => e,
                },
            };
            reached &= probe.ok;
            let record = Command::RecordDrillProbe {
                drill: id.to_string(),
                channel: channel.clone(),
                probe,
            };
            self.tell(format!("drill-{id}-{channel}"), record).await?;
        }
        self.tell(format!("drill-{id}-close"), Command::CloseDrill { drill: id.to_string() }).await?;
        Ok(reached)
    }
}

/// The pump as deployed: the cluster, the banks over HTTPS, Key Vault for
/// signing, immutable Blob Storage for evidence, mounted bank credentials.
pub type DeployedPump = Pump<
    ic_raft::ClusterClient,
    crate::ports::ClusterCore,
    crate::bank::HttpBank,
    ic_azure::blob::BlobContainer,
    crate::ports::VaultKeys,
    crate::ports::MountedSecrets,
    crate::ports::LogNotifier,
>;

/// Structured logs on standard error, at `info` unless `RUST_LOG` says
/// otherwise. A second call leaves the first in place.
pub fn log_to_stderr() {
    let _ = tracing_subscriber::fmt()
        .json()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .try_init();
}

/// Resolves when the process is asked to stop.
pub async fn interrupted() {
    let _ = tokio::signal::ctrl_c().await;
}

/// The whole of the binary: settings and the Azure identity from the
/// environment `get` reads, then [`Pump::run`].
pub async fn run_from(args: &[String], get: impl Fn(&str) -> Option<String>, stop: impl std::future::Future<Output = ()>) -> Result<(), String> {
    let settings = crate::settings::PumpSettings::from_lookup(&get)?;
    let credential = ic_azure::credential::AzureCredential::from_lookup(&get).map_err(|e| e.to_string())?;
    DeployedPump::deployed(&settings, credential).run(args, settings.interval, stop).await
}

impl DeployedPump {
    pub fn deployed(settings: &crate::settings::PumpSettings, credential: ic_azure::credential::AzureCredential) -> Self {
        let client = ic_raft::ClusterClient::new(settings.nodes.clone()).with_auth(settings.auth.clone());
        Pump {
            module: client.clone(),
            core: crate::ports::ClusterCore {
                client,
                matcher: ic_cashmatch::Matcher::default(),
            },
            bank: crate::bank::HttpBank::default(),
            vault: ic_azure::blob::BlobContainer::new(&settings.blob_endpoint, &settings.container, credential.clone()),
            keys: crate::ports::VaultKeys {
                vault: settings.key_vault.clone(),
                credential,
            },
            secrets: crate::ports::MountedSecrets(settings.secrets_dir.clone()),
            notifier: crate::ports::LogNotifier,
            region: settings.region.clone(),
            instance: settings.instance.clone(),
            clock: Arc::new(nz_now),
            grace: settings.grace,
            cursors: Mutex::default(),
        }
    }
}

impl<M: Module, C: Core, B: Bank, V: WormStore, K: Keys, S: Secrets, N: Notifier> Pump<M, C, B, V, K, S, N> {
    async fn drill_named(&self, id: &str) -> Result<(), String> {
        match self.drill(id).await? {
            true => Ok(()),
            false => Err(format!("drill {id} from {}: a bank channel could not be reached", self.region)),
        }
    }

    /// What the binary does with its arguments: nothing for the standing
    /// pump (until `stop` resolves), `once` for one pass that fails if
    /// anything was left undone, `drill [<id>]` for the evacuation drill.
    pub async fn run(&self, args: &[String], interval: std::time::Duration, stop: impl std::future::Future<Output = ()>) -> Result<(), String> {
        match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            [] => {
                tokio::pin!(stop);
                loop {
                    let pass = self.pass().await;
                    tracing::info!(
                        sent = pass.sent,
                        failed = pass.failed,
                        messages = pass.messages,
                        instructions = pass.instructions,
                        "pass"
                    );
                    for problem in &pass.problems {
                        tracing::warn!(target: "payments.pump", problem, "not done this pass");
                    }
                    tokio::select! {
                        () = tokio::time::sleep(interval) => {}
                        () = &mut stop => return Ok(()),
                    }
                }
            }
            ["once"] => {
                let pass = self.pass().await;
                if pass.problems.is_empty() {
                    Ok(())
                } else {
                    Err(pass.problems.join("; "))
                }
            }
            // The scheduled drill names itself from the day and the region,
            // so a job that is retried the same day records one drill.
            ["drill"] => self.drill_named(&format!("{}-{}", (self.clock)().date(), self.region)).await,
            ["drill", id] => self.drill_named(id).await,
            _ => Err("usage: ic-payments-pump [once | drill [<id>]]".to_string()),
        }
    }
}

/// The hash of what was, or would have been, sent. A batch its format
/// cannot carry was never sent, and says so in place of a hash.
fn manifest_hash(batch: &Batch) -> String {
    match batch.content_hash() {
        Ok(hash) => hash,
        Err(e) => format!("unwritten: {e}"),
    }
}
