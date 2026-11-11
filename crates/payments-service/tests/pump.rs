//! The pump against a bank in this process and the real core ledger: files
//! and API requests go out, the bank's answers come back, the ledger moves
//! when the bank says the money moved, and every way a bank or a network
//! can fail leaves the money accounted for.

#[path = "../../payments/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration, NaiveDateTime};
use common::*;
use ic_azure::blob::{MemoryWormStore, WormStore};
use ic_azure::keyvault::Signature;
use ic_azure::AzureError;
use ic_cashmatch::IncomingPayment;
use ic_core::{accounts, GroupId, PayableKind, PolicyId};
use ic_log::StateMachine;
use ic_money::Money;
use ic_payments::file::BankMessage;
use ic_payments::model::{
    Counterparty, ExceptionKind, NewItem, PayeeKind, RefundDestination, RefundStatus, ReturnReason, RunType, StatementLine, SubmissionStatus,
};
use ic_payments::{Actor, Command, Envelope, Output, PaymentsState, Role};
use ic_payments_service::bank::{Bank, ChannelError, HttpBank, Target};
use ic_payments_service::fake::FakeBank;
use ic_payments_service::ports::{policy_of, Core, Keys, LogNotifier, Module, MountedSecrets, Notifier, Secrets};
use ic_payments_service::pump::{nz_now, Pump};
use ic_product::{Frequency, Sex};
use ic_sim::config::shipped_activations;
use ic_sim::persona::{issue_command, term_life};
use ic_sim::Cluster;
use serde_json::Value;

const MONTHLY: i64 = 61_84;

/// The module in this process, behind the same seam a cluster offers.
#[derive(Clone, Default)]
struct Local {
    state: Arc<Mutex<(PaymentsState, u64)>>,
    down: Arc<AtomicBool>,
}

impl Module for Local {
    async fn propose(&self, envelope: Envelope) -> Result<Output, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("no leader".to_string());
        }
        let mut guard = self.state.lock().unwrap();
        guard.1 += 1;
        let index = guard.1;
        Ok(guard.0.apply(index, envelope))
    }
    async fn query(&self, question: Value) -> Result<Value, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("no leader".to_string());
        }
        ic_payments::query::answer(&self.state.lock().unwrap().0, question)
    }
}

/// The real core state machine, one partition, behind the pump's seam.
#[derive(Clone)]
struct SimCore {
    cluster: Arc<Mutex<Cluster>>,
    down: Arc<AtomicBool>,
    /// What cash matching would find: reference to policy.
    known: Arc<Mutex<BTreeMap<String, PolicyId>>>,
}

impl Core for SimCore {
    async fn send(&self, key: &str, caller: &str, command: ic_core::Command) -> Result<Option<String>, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("the core cannot be reached".to_string());
        }
        assert!(policy_of(&command).is_some(), "the pump only sends commands about a policy");
        let applied = self.cluster.lock().unwrap().submit_keyed(0, key, caller, command);
        Ok(applied.result.err().map(|r| r.code))
    }
    async fn match_receipt(&self, payment: IncomingPayment) -> Result<Option<String>, String> {
        let policy = payment.reference.as_ref().and_then(|r| self.known.lock().unwrap().get(r).cloned());
        match policy {
            Some(policy) => {
                let command = ic_core::Command::ApplyReceipt {
                    policy,
                    receipt: payment.id.clone(),
                    amount: payment.amount,
                };
                self.send(&payment.id, "cash-matching", command).await
            }
            None => Ok(Some("no_match".to_string())),
        }
    }
}

#[derive(Clone, Default)]
struct TestKeys {
    broken: Arc<AtomicBool>,
}

impl Keys for TestKeys {
    async fn sign(&self, key: &str, message: &[u8]) -> Result<Signature, String> {
        if self.broken.load(Ordering::SeqCst) {
            return Err("the HSM is unavailable".to_string());
        }
        Ok(Signature {
            algorithm: "test".to_string(),
            key_id: key.to_string(),
            value: format!("signed-{}", message.len()),
        })
    }
}

/// A vault that can be made to refuse writes.
#[derive(Clone, Default)]
struct Vault {
    store: MemoryWormStore,
    broken: Arc<AtomicBool>,
}

impl WormStore for Vault {
    async fn put_new(&self, name: &str, content_type: &str, body: Vec<u8>) -> Result<(), AzureError> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(AzureError::Response("the vault is unavailable".to_string()));
        }
        self.store.put_new(name, content_type, body).await
    }
    async fn get(&self, name: &str) -> Result<Option<Vec<u8>>, AzureError> {
        self.store.get(name).await
    }
}

#[derive(Clone, Default)]
struct Notices {
    sent: Arc<Mutex<Vec<(Role, String, String)>>>,
    broken: Arc<AtomicBool>,
}

impl Notifier for Notices {
    async fn notify(&self, to: Role, subject: &str, detail: &str) -> Result<(), String> {
        if self.broken.load(Ordering::SeqCst) {
            return Err("nobody could be told".to_string());
        }
        self.sent.lock().unwrap().push((to, subject.to_string(), detail.to_string()));
        Ok(())
    }
}

type TestPump = Pump<Local, SimCore, HttpBank, Vault, TestKeys, BTreeMap<String, String>, Notices>;

struct World {
    module: Local,
    core: SimCore,
    /// The first bank's file gateway.
    bank: FakeBank,
    /// The first bank's API.
    api: FakeBank,
    /// The second bank's file gateway.
    second: FakeBank,
    vault: Vault,
    keys: TestKeys,
    notices: Notices,
    now: Arc<Mutex<NaiveDateTime>>,
    sent: u64,
    policy: PolicyId,
}

impl World {
    /// A configured module whose channels all lead to one fake bank, which
    /// has whitelisted Auckland and Sydney but not Melbourne, and a core
    /// with one policy that has overpaid by 100.00.
    async fn new() -> Self {
        let bank = FakeBank::start(&["token-akl", "token-syd"]).await.unwrap();
        let api = FakeBank::start(&["token-akl", "token-syd"]).await.unwrap();
        let second = FakeBank::start(&["token-akl", "token-syd"]).await.unwrap();
        let mut cluster = Cluster::in_memory(1, 42);
        cluster.open(d(2027, 11, 15)).unwrap();
        for cmd in shipped_activations().unwrap() {
            cluster.activate_everywhere(cmd).unwrap();
        }
        let n = (1..).find(|n| cluster.partition_of(&GroupId(format!("grp-{n:08}"))) == 0).unwrap();
        let issue = term_life(
            n,
            d(2027, 11, 15),
            d(1990, 3, 10),
            Sex::Female,
            false,
            500_000_00,
            None,
            Frequency::Monthly,
            None,
        );
        let policy = issue.policy.clone();
        cluster.submit(0, "new-business", issue_command(issue)).unwrap();
        let receipt = ic_core::Command::ApplyReceipt {
            policy: policy.clone(),
            receipt: "first".into(),
            amount: Money::new(MONTHLY + 100_00, NZD),
        };
        cluster.submit(0, "cash-matching", receipt).unwrap();

        let mut world = Self {
            module: Local::default(),
            core: SimCore {
                cluster: Arc::new(Mutex::new(cluster)),
                down: Arc::default(),
                known: Arc::default(),
            },
            bank,
            api,
            second,
            vault: Vault::default(),
            keys: TestKeys::default(),
            notices: Notices::default(),
            now: Arc::new(Mutex::new(monday())),
            sent: 0,
            policy,
        };
        let mut config = config();
        for (id, channel) in &mut config.channels {
            let url = match id.as_str() {
                "anz-file" => &world.bank.url,
                "anz-api" => &world.api.url,
                _ => &world.second.url,
            };
            for (region, endpoint) in &mut channel.endpoints {
                endpoint.url = format!("{url}/");
                endpoint.credential = format!("bank-{region}");
            }
        }
        world.ok(
            &cal(),
            Command::ActivateConfig {
                version: 1,
                config: Box::new(config),
            },
        );
        for code in ["anz-sf1-claims", "anz-sf1-refunds", "anz-sf1-operating", "anz-sf1-collection"] {
            let statement = Command::RecordStatement {
                account: code.to_string(),
                date: d(2027, 11, 12),
                opening_minor: 1_000_000_00,
                closing_minor: 1_000_000_00,
                lines: vec![],
            };
            world.ok(&svc(), statement);
        }
        world
    }

    fn pump(&self, region: &str, instance: &str) -> TestPump {
        let now = self.now.clone();
        let secrets = REGIONS.iter().map(|r| (format!("bank-{r}"), format!("token-{r}"))).collect();
        Pump {
            module: self.module.clone(),
            core: self.core.clone(),
            bank: HttpBank::default(),
            vault: self.vault.clone(),
            keys: self.keys.clone(),
            secrets,
            notifier: self.notices.clone(),
            region: region.to_string(),
            instance: instance.to_string(),
            clock: Arc::new(move || *now.lock().unwrap()),
            grace: Duration::minutes(10),
            cursors: Mutex::default(),
        }
    }

    fn send(&mut self, who: &Actor, command: Command) -> Output {
        self.sent += 1;
        let envelope = Envelope {
            key: format!("person-{}", self.sent),
            actor: who.clone(),
            at: *self.now.lock().unwrap(),
            command,
        };
        let mut guard = self.module.state.lock().unwrap();
        guard.1 += 1;
        let index = guard.1;
        guard.0.apply(index, envelope)
    }

    fn ok(&mut self, who: &Actor, command: Command) {
        if let Err(reject) = self.send(who, command) {
            panic!("refused: {} ({})", reject.code, reject.message);
        }
    }

    fn state(&self) -> PaymentsState {
        self.module.state.lock().unwrap().0.clone()
    }

    fn later(&self, minutes: i64) {
        *self.now.lock().unwrap() += Duration::minutes(minutes);
    }

    fn payee(&mut self, id: &str, kind: PayeeKind) {
        let register = Command::RegisterPayee {
            payee: id.to_string(),
            party: "party".to_string(),
            kind,
            account: account(id, "0101234567"),
            name_check: ic_payments::model::NameCheck::Match,
        };
        self.ok(&pam(), register);
        self.ok(&pat(), Command::ConfirmPayee { payee: id.to_string() });
        self.ok(
            &svc(),
            Command::ScreenPayee {
                payee: id.to_string(),
                clear: true,
            },
        );
    }

    /// A checked and approved run of one type with these items.
    fn run(&mut self, id: &str, run_type: RunType, urgent: bool, items: Vec<NewItem>) -> String {
        let today = self.now.lock().unwrap().date();
        let prepare = Command::PrepareRun {
            run: id.to_string(),
            run_type,
            fund: "sf1".to_string(),
            currency: NZD,
            value_date: today,
            urgent,
        };
        self.ok(&mia(), prepare);
        self.ok(&mia(), Command::AddRunItems { run: id.to_string(), items });
        self.ok(&mia(), Command::CheckRun { run: id.to_string() });
        self.ok(&ari(), Command::ApproveRun { run: id.to_string() });
        if urgent {
            self.ok(&ben(), Command::ApproveRun { run: id.to_string() });
        }
        format!("{id}-1")
    }

    /// The policy's suspense, its refund payable, and the mirror of a bank account.
    fn ledger(&self, bank_account: &str) -> (i128, i128, i128) {
        let cluster = self.core.cluster.lock().unwrap();
        let l = &cluster.state(0).ledger;
        (
            l.natural_balance(&accounts::policy_suspense(&self.policy, NZD).id),
            l.natural_balance(&accounts::refund_payable(&self.policy, NZD).id),
            l.balance(&accounts::bank(bank_account, NZD).id),
        )
    }
}

/// Purpose: a premium refund travels the whole way: asked of the core, owed
/// in the ledger, sent to the bank as a signed file with its evidence in the
/// vault, acknowledged, settled into the ledger account that mirrors the
/// bank account, then returned by the bank and owed again, with the core's
/// own reconciliation clean throughout.
/// Why it matters: this is the module, the channel, the bank and the real
/// ledger working as one. Each part passing its own tests does not show
/// that money owed, money at the bank and money in the ledger stay in step.
#[tokio::test]
async fn a_refund_travels_from_the_ledger_to_the_bank_and_back() {
    let mut world = World::new().await;
    let pump = world.pump("akl", "a");
    let policy = world.policy.clone();
    let create = Command::CreateAuthority {
        authority: "a1".to_string(),
        policy: policy.clone(),
        party: "payer".to_string(),
        account: account("Ana Payer", "0101234567"),
        sole_name: false,
    };
    world.ok(&pam(), create);
    let request = Command::RequestRefund {
        refund: "rf1".to_string(),
        policy: policy.clone(),
        amount_minor: 100_00,
        currency: NZD,
        destination: RefundDestination::Source { authority: "a1".to_string() },
    };
    world.ok(&mia(), request);
    assert_eq!(world.ledger("anz-sf1-refunds"), (100_00, 0, 0));
    let pass = pump.pass().await;
    assert_eq!((pass.instructions, pass.problems.len()), (1, 0), "{:?}", pass.problems);
    assert_eq!(world.ledger("anz-sf1-refunds"), (0, 100_00, 0));
    assert_eq!(world.state().refunds["rf1"].status, RefundStatus::Raised);

    let item = NewItem {
        item: "i1".to_string(),
        source: "refund-rf1".to_string(),
        counterparty: Counterparty::Authority("a1".to_string()),
        amount_minor: 100_00,
        policy: Some(policy.clone()),
        payable: Some(PayableKind::Refund),
        refund: Some("rf1".to_string()),
        represents: None,
    };
    let s = world.run("r1", RunType::PremiumRefunds, false, vec![item]);
    let pass = pump.pass().await;
    assert_eq!((pass.sent, pass.failed, pass.problems.len()), (1, 0, 0), "{:?}", pass.problems);
    let state = world.state();
    assert_eq!(state.submissions[&s].status, SubmissionStatus::Sent);
    assert_eq!(state.submissions[&s].bank_reference.as_deref(), Some("BANK-1"));
    let held = world.bank.lock().held[&s].clone();
    assert_eq!(held.items, vec![("i1".to_string(), 100_00)]);
    assert_eq!(Some(&held.sha256), state.submissions[&s].content_hash.as_ref());
    // The vault holds exactly what the bank was sent.
    let file = world.vault.get("payments/r1-1/file.txt").await.unwrap().unwrap();
    assert_eq!(String::from_utf8(file).unwrap(), state.batch(&s).unwrap().file());
    let manifest: Value = serde_json::from_slice(&world.vault.get("payments/r1-1/manifest.json").await.unwrap().unwrap()).unwrap();
    assert_eq!(manifest["key_id"], "anz-file-akl-signing");
    assert_eq!(manifest["sha256"], held.sha256);

    // Nothing has settled: the ledger has not moved.
    assert_eq!(world.ledger("anz-sf1-refunds"), (0, 100_00, 0));
    world.bank.acknowledge(&s);
    world.bank.settle(&s, d(2027, 11, 15));
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.instructions, pass.problems.len()), (2, 1, 0), "{:?}", pass.problems);
    assert_eq!(world.ledger("anz-sf1-refunds"), (0, 0, -100_00));
    assert_eq!(world.state().refunds["rf1"].status, RefundStatus::Paid);
    assert!(world.core.cluster.lock().unwrap().reconcile().is_empty());

    world.bank.push(BankMessage::Return {
        item: "i1".to_string(),
        reason: ReturnReason::AccountClosed,
        returned_on: d(2027, 11, 16),
    });
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.instructions), (1, 2), "{:?}", pass.problems);
    assert_eq!(world.ledger("anz-sf1-refunds"), (0, 100_00, 0));
    assert_eq!(world.state().refunds["rf1"].status, RefundStatus::Raised);
    assert!(world.core.cluster.lock().unwrap().reconcile().is_empty());
    assert!(world
        .notices
        .sent
        .lock()
        .unwrap()
        .iter()
        .any(|(to, subject, _)| *to == Role::Maker && subject == "payment_returned"));
    // A pass with nothing to do does nothing, and reads no message twice.
    let pass = pump.pass().await;
    assert_eq!(pass, Default::default());
    assert_eq!(world.bank.lock().arrivals, 1);
}

/// Purpose: an urgent run goes as one API request, the bank's answer in the
/// same response is recorded as the acknowledgement, and an item the bank
/// refuses there is rejected at once.
/// Why it matters: the urgent path exists for a claim that must be paid
/// tonight. Its result must be known tonight, not when a file comes back.
#[tokio::test]
async fn an_urgent_run_is_acknowledged_in_the_same_request() {
    let mut world = World::new().await;
    let pump = world.pump("syd", "a");
    world.payee("p1", PayeeKind::Beneficiary);
    world.api.lock().reject_items.insert("i2".to_string(), "account_invalid".to_string());
    let s = world.run("u1", RunType::LumpSumClaims, true, vec![pay("i1", "p1", 9_000_00), pay("i2", "p1", 8_000_00)]);
    let pass = pump.pass().await;
    assert_eq!((pass.sent, pass.problems.len()), (1, 0), "{:?}", pass.problems);
    let state = world.state();
    assert_eq!(state.submissions[&s].channel, "anz-api");
    assert_eq!(state.submissions[&s].region.as_deref(), Some("syd"));
    assert_eq!(state.submissions[&s].status, SubmissionStatus::Acknowledged);
    assert_eq!(state.control_totals("u1").unwrap()["rejected"], 8_000_00);
    assert_eq!(state.control_totals("u1").unwrap()["awaiting_settlement"], 9_000_00);
}

/// Purpose: while the bank is down a submission stays queued and nothing is
/// taken; when the bank refuses a file outright the submission fails and
/// its items are unpaid again; a region the bank has not whitelisted cannot
/// take anything.
/// Why it matters: a queue that survives an outage and a refusal that is
/// recorded as a refusal are what let the same run be paid later, or
/// through the second bank, exactly once.
#[tokio::test]
async fn a_bank_that_is_down_or_says_no_leaves_the_money_unpaid_and_known() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);

    world.bank.lock().down = true;
    let pass = world.pump("akl", "a").pass().await;
    assert_eq!((pass.sent, pass.failed), (0, 0));
    assert!(pass.problems.iter().any(|p| p.contains("submission r1-1")), "{:?}", pass.problems);
    assert_eq!(world.state().submissions[&s].status, SubmissionStatus::Queued);
    world.bank.lock().down = false;

    let pass = world.pump("mel", "m").pass().await;
    assert!(pass.problems.iter().any(|p| p.contains("allow list")), "{:?}", pass.problems);
    assert_eq!(world.state().submissions[&s].status, SubmissionStatus::Queued);

    world.bank.lock().refuse_next = Some("signature_invalid".to_string());
    let pass = world.pump("akl", "a").pass().await;
    assert_eq!((pass.sent, pass.failed), (0, 1), "{:?}", pass.problems);
    let state = world.state();
    assert!(matches!(&state.submissions[&s].status, SubmissionStatus::Failed(reason) if reason.contains("signature_invalid")));
    assert!(world.bank.lock().held.is_empty());
    // A person sends what is unpaid again, as a new file.
    world.ok(
        &mia(),
        Command::Resubmit {
            run: "r1".to_string(),
            value_date: d(2027, 11, 15),
        },
    );
    let pass = world.pump("akl", "a").pass().await;
    assert_eq!(pass.sent, 1, "{:?}", pass.problems);
    assert!(world.bank.lock().held.contains_key("r1-2"));
}

/// Purpose: when a file is sent and no clear answer comes back, the pump
/// does nothing until the grace period has passed, then asks the bank: a
/// file the bank holds is recorded as sent without sending it again, and a
/// file the bank does not hold fails so that its items can go in a new one.
/// Why it matters: this is the case that pays a whole run twice when it is
/// handled by retrying. The rule is to ask the bank, never to resend.
#[tokio::test]
async fn an_unclear_answer_is_resolved_by_asking_the_bank() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let pump = world.pump("akl", "a");
    let arrived = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    world.bank.lock().swallow_next = true;
    let pass = pump.pass().await;
    assert!(pass.problems.iter().any(|p| p.contains("no clear answer")), "{:?}", pass.problems);
    assert_eq!(world.state().submissions[&arrived].status, SubmissionStatus::Sending);

    let lost = world.run("r2", RunType::BenefitPayments, false, vec![pay("i2", "p1", 400_00)]);
    world.bank.lock().drop_next = true;
    pump.pass().await;
    assert_eq!(world.state().submissions[&lost].status, SubmissionStatus::Sending);

    // Inside the grace period nobody touches them, from any region.
    world.later(5);
    let pass = world.pump("syd", "b").pass().await;
    assert_eq!((pass.sent, pass.failed), (0, 0));
    assert_eq!(world.bank.lock().arrivals, 2);

    world.later(6);
    let pass = world.pump("syd", "b").pass().await;
    assert_eq!((pass.sent, pass.failed), (1, 1), "{:?}", pass.problems);
    let state = world.state();
    assert_eq!(state.submissions[&arrived].status, SubmissionStatus::Sent);
    assert!(matches!(&state.submissions[&lost].status, SubmissionStatus::Failed(reason) if reason.contains("does not hold")));
    assert_eq!(world.bank.lock().arrivals, 2, "nothing was sent a second time");

    // If the bank cannot be asked, the question waits.
    let unsure = world.run("r3", RunType::BenefitPayments, false, vec![pay("i3", "p1", 500_00)]);
    world.bank.lock().swallow_next = true;
    pump.pass().await;
    world.later(11);
    world.bank.lock().down = true;
    let pass = pump.pass().await;
    assert!(pass.problems.iter().any(|p| p.contains("submission r3-1")), "{:?}", pass.problems);
    assert_eq!(world.state().submissions[&unsure].status, SubmissionStatus::Sending);
}

/// Purpose: two pumps that go for the same submission do not both send it,
/// and a pump that finds the bank already holds a queued submission records
/// it as sent instead of sending it.
/// Why it matters: pumps run in every region at once so that any region can
/// submit. Without one claim per submission that is a double payment.
#[tokio::test]
async fn two_pumps_never_send_the_same_submission() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    let (a, b) = (world.pump("akl", "a"), world.pump("syd", "b"));
    let (first, second) = tokio::join!(a.pass(), b.pass());
    assert_eq!(first.sent + second.sent, 1, "{first:?} {second:?}");
    assert_eq!(world.bank.lock().held.len(), 1);
    assert_eq!(world.state().submissions[&s].status, SubmissionStatus::Sent);

    // A pump died after the bank took the file and before it told the
    // module, and the queue was restored from an older snapshot.
    let s2 = world.run("r2", RunType::BenefitPayments, false, vec![pay("i2", "p1", 400_00)]);
    let stray = world.pump("akl", "c");
    let config: ic_payments::Config = serde_json::from_value(world.module.query(serde_json::json!({"q": "config"})).await.unwrap()).unwrap();
    let endpoint = &config.channels["anz-file"].endpoints["akl"];
    let to = Target {
        kind: config.channels["anz-file"].kind,
        url: endpoint.url.trim_end_matches('/').to_string(),
        token: "token-akl".to_string(),
    };
    let batch = world.state().batch(&s2).unwrap();
    let manifest = batch.manifest("test".to_string(), "k".to_string(), "sig".to_string());
    HttpBank::default().submit(&to, &batch, &manifest).await.unwrap();
    let pass = stray.pass().await;
    assert_eq!(pass.sent, 1, "{:?}", pass.problems);
    assert_eq!(world.bank.lock().arrivals, 2, "the pump did not send what the bank already held");
    assert_eq!(world.state().submissions[&s2].status, SubmissionStatus::Sent);
}

/// Purpose: the bank itself refuses a file whose manifest does not match,
/// whose trailer is wrong, or that reuses an ID for other content, answers
/// a repeat of the same file as the first time, and tells a caller it has
/// not whitelisted to go away.
/// Why it matters: the test double has to check what a bank checks, or the
/// tests above prove nothing about what a bank would accept.
#[tokio::test]
async fn the_fake_bank_checks_what_a_bank_checks() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    let batch = world.state().batch(&s).unwrap();
    let good = batch.manifest("test".to_string(), "k".to_string(), "sig".to_string());
    let file = |token: &str| Target {
        kind: ic_payments::config::ChannelKind::File,
        url: world.bank.url.clone(),
        token: token.to_string(),
    };
    let api = Target {
        kind: ic_payments::config::ChannelKind::Api,
        ..file("token-akl")
    };
    let bank = HttpBank::default();
    assert!(matches!(bank.submit(&file("token-mel"), &batch, &good).await, Err(ChannelError::Refused(r)) if r.starts_with("403")));
    assert!(matches!(bank.messages(&file("nope"), 0).await, Err(ChannelError::Refused(_))));
    assert!(matches!(bank.holds(&file("nope"), "r1-1").await, Err(ChannelError::Refused(_))));
    let mut unsigned = good.clone();
    unsigned.signature = String::new();
    assert!(matches!(bank.submit(&file("token-akl"), &batch, &unsigned).await, Err(ChannelError::Refused(r)) if r.contains("manifest")));
    let first = bank.submit(&file("token-akl"), &batch, &good).await.unwrap();
    let again = bank.submit(&file("token-akl"), &batch, &good).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(bank.holds(&file("token-akl"), "r1-1").await.unwrap(), Some(batch.content_hash()));
    assert_eq!(bank.holds(&api, "r9-9").await.unwrap(), None);
    let mut other = batch.clone();
    other.items[0].amount_minor = 999_00;
    let manifest = other.manifest("test".to_string(), "k".to_string(), "sig".to_string());
    assert!(matches!(bank.submit(&file("token-akl"), &other, &manifest).await, Err(ChannelError::Refused(r)) if r.starts_with("409")));
    // The same goes for the API: the same request twice is one batch.
    let mut by_api = batch.clone();
    by_api.id = "api-1".to_string();
    let first = bank.submit(&api, &by_api, &good).await.unwrap();
    assert!(first.acknowledgement.is_some());
    assert_eq!(bank.submit(&api, &by_api, &good).await.unwrap(), first);
    assert_eq!(world.bank.lock().held.len(), 2);
    // Raw requests a well-behaved client never makes.
    let http = reqwest::Client::new();
    let put = |body: &'static str, manifest: Option<String>| {
        let mut request = http.put(format!("{}/files/x1", world.bank.url)).bearer_auth("token-akl").body(body);
        if let Some(m) = manifest {
            request = request.header("x-manifest", m);
        }
        request.send()
    };
    assert_eq!(put("H|x\nT|0|0\n", None).await.unwrap().status(), 422);
    use base64::Engine;
    let text = "H|ICPAY1|x1|a|CREDIT|2027-11-15|NZD\nD|i|n|0101234567|500\nT|2|500\n";
    let mut m = good.clone();
    m.file_id = "x1".to_string();
    m.sha256 = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(text.as_bytes()));
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&m).unwrap());
    assert_eq!(put(text, Some(encoded)).await.unwrap().status(), 422);
    let wrong_key = http
        .post(format!("{}/batches", world.bank.url))
        .bearer_auth("token-akl")
        .header("idempotency-key", "zz")
        .json(&by_api);
    assert_eq!(wrong_key.send().await.unwrap().status(), 400);
    // A bank that is not there at all was never sent anything.
    let nowhere = Target {
        url: "http://127.0.0.1:1".to_string(),
        ..file("token-akl")
    };
    assert!(matches!(bank.submit(&nowhere, &batch, &good).await, Err(ChannelError::NotSent(_))));
    world.bank.lock().down = true;
    assert!(matches!(bank.messages(&file("token-akl"), 0).await, Err(ChannelError::Unknown(_))));
    // Acknowledging something the bank never held says nothing.
    world.bank.acknowledge("never");
    assert!(world.bank.lock().messages.is_empty());
}

/// Purpose: when the core cannot be reached the outbox waits, in order, and
/// is worked on the next pass; when the core refuses an instruction the
/// refusal is recorded and becomes an exception.
/// Why it matters: a return must never reach the core before its
/// settlement, and a settlement the ledger will not take is a break between
/// the bank and the books that someone has to see.
#[tokio::test]
async fn the_outbox_waits_for_the_core_and_records_its_refusals() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Beneficiary);
    let pump = world.pump("akl", "a");
    // A benefit the core does not owe: the ledger will refuse to settle it.
    let mut item = pay("i1", "p1", 300_00);
    item.policy = Some(world.policy.clone());
    item.payable = Some(PayableKind::Benefit);
    let s = world.run("r1", RunType::LumpSumClaims, false, vec![item]);
    pump.pass().await;
    world.bank.acknowledge(&s);
    world.bank.settle(&s, d(2027, 11, 15));
    world.bank.push(BankMessage::Return {
        item: "i1".to_string(),
        reason: ReturnReason::Other("recalled".to_string()),
        returned_on: d(2027, 11, 16),
    });
    world.core.down.store(true, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.instructions), (3, 0));
    assert!(pass.problems.iter().any(|p| p.contains("outbox settle-i1")), "{:?}", pass.problems);
    assert_eq!(world.state().outbox.len(), 3);

    world.core.down.store(false, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert_eq!(pass.instructions, 3, "{:?}", pass.problems);
    let state = world.state();
    assert!(state.outbox.is_empty());
    let refused: Vec<_> = state
        .exceptions
        .values()
        .filter(|e| e.kind == ExceptionKind::CoreRefused)
        .map(|e| e.reference.clone())
        .collect();
    assert_eq!(refused, vec!["settle-i1: insufficient_payable", "return-i1: unknown_payment"]);
}

/// Purpose: a statement from the bank is reconciled; a direct credit on it
/// goes to cash matching and is applied to the policy its reference names,
/// and one nobody can place stays open; a message the module will not take
/// is reported to a reconciler and not read again.
/// Why it matters: this is the inbound side for money customers send
/// themselves, and the place a disagreement with the bank first shows.
#[tokio::test]
async fn statements_and_direct_credits_come_in_through_the_bank() {
    let world = World::new().await;
    let pump = world.pump("akl", "a");
    world.core.known.lock().unwrap().insert("POLICY REF".to_string(), world.policy.clone());
    let line = |id: &str, amount: i64, reference: &str| StatementLine {
        id: id.to_string(),
        amount_minor: amount,
        reference: reference.to_string(),
        payer_account: None,
    };
    world.bank.push(BankMessage::Statement {
        account: "anz-sf1-collection".to_string(),
        date: d(2027, 11, 15),
        opening_minor: 1_000_000_00,
        closing_minor: 1_000_075_00,
        lines: vec![line("l1", 50_00, "POLICY REF"), line("l2", 25_00, "thanks")],
    });
    world.bank.push(BankMessage::Settlement {
        id: "never-sent".to_string(),
        settled_on: d(2027, 11, 15),
    });
    let before = world.ledger("x").0;
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.instructions), (1, 2), "{:?}", pass.problems);
    assert!(
        pass.problems.iter().any(|p| p.contains("message 2") && p.contains("unknown_submission")),
        "{:?}",
        pass.problems
    );
    assert_eq!(world.ledger("x").0, before + 50_00, "the identified credit is on the policy");
    let open: Vec<_> = world
        .state()
        .exceptions
        .values()
        .filter(|e| e.resolution.is_none())
        .map(|e| e.reference.clone())
        .collect();
    assert_eq!(open, vec!["thanks"]);
    let notices = world.notices.sent.lock().unwrap().clone();
    assert!(notices
        .iter()
        .any(|(to, subject, _)| *to == Role::Reconciler && subject == "bank_message_refused"));
    // The refused message is not read again.
    assert_eq!(pump.pass().await, Default::default());

    // If nobody can be told either, that is reported too.
    world.notices.broken.store(true, Ordering::SeqCst);
    world.bank.push(BankMessage::Settlement {
        id: "never-sent-2".to_string(),
        settled_on: d(2027, 11, 15),
    });
    let pass = pump.pass().await;
    assert!(pass.problems.iter().any(|p| p.contains("nobody could be told")), "{:?}", pass.problems);
}

/// Purpose: the evacuation drill sends a token payment through every
/// channel from one region, as a file where the channel takes files and an
/// API request where it takes those, keeps the evidence, and passes only if
/// the bank took all of them; from a region the bank has not whitelisted it
/// fails and says which channels could not be reached; a drill repeated
/// under the same ID changes nothing.
/// Why it matters: the addendum requires that a direct debit file and an
/// urgent payment can be submitted from whichever region holds the leaders,
/// and that the drill proves it with real submissions. A region is not
/// ready until this has passed from it.
#[tokio::test]
async fn the_drill_proves_a_region_can_reach_every_channel() {
    let world = World::new().await;
    let sydney = world.pump("syd", "s");
    assert!(sydney.drill("d1").await.unwrap());
    for (bank, id) in [
        (&world.bank, "drill-d1-anz-file"),
        (&world.api, "drill-d1-anz-api"),
        (&world.second, "drill-d1-bnz-file"),
    ] {
        assert_eq!(bank.lock().held.keys().collect::<Vec<_>>(), vec![id]);
    }
    assert_eq!(
        world.bank.lock().held["drill-d1-anz-file"].items,
        vec![("drill-d1-anz-file-1".to_string(), 1_00)]
    );
    assert!(world.vault.get("payments/drill-d1-bnz-file/manifest.json").await.unwrap().is_some());
    let state = world.state();
    assert_eq!(state.drills["d1"].passed, Some(true));
    let ready: BTreeMap<String, bool> = state.readiness(d(2027, 11, 15)).into_iter().map(|r| (r.region, r.ready)).collect();
    assert_eq!(
        ready,
        BTreeMap::from([("akl".to_string(), false), ("mel".to_string(), false), ("syd".to_string(), true)])
    );

    let melbourne = world.pump("mel", "m");
    assert!(!melbourne.drill("d2").await.unwrap());
    let state = world.state();
    assert_eq!(state.drills["d2"].passed, Some(false));
    assert!(state.drills["d2"].probes.values().all(|p| !p.ok && p.detail.contains("allow list")));
    assert!(!state.readiness(d(2027, 11, 15)).iter().any(|r| r.region == "mel" && r.ready));
    // Run again after a crash, the same drill gives the same answer and
    // the bank holds nothing new.
    assert!(sydney.drill("d1").await.unwrap());
    assert_eq!(world.bank.lock().held.len(), 1);

    // The bank's answers to the drill's token batches belong to no run and
    // are passed over without a complaint.
    world.bank.acknowledge("drill-d1-anz-file");
    world.bank.settle("drill-d1-anz-file", d(2027, 11, 15));
    let pass = sydney.pass().await;
    assert_eq!((pass.messages, pass.problems.len()), (0, 0), "{:?}", pass.problems);
    // A pump in a region with no credential cannot even try.
    let mut nowhere = world.pump("akl", "x");
    nowhere.secrets.clear();
    assert!(!nowhere.drill("d3").await.unwrap());
    assert!(world.state().drills["d3"].probes.values().all(|p| p.detail.contains("credential")));
}

/// Purpose: nothing is sent when the signing key or the evidence vault is
/// unavailable, and the submission fails cleanly so it can be sent again;
/// a pump with no module, or a module with no configuration, reports that
/// and does nothing.
/// Why it matters: a payment file the vault does not hold is a payment
/// nobody can later prove was the one approved.
#[tokio::test]
async fn nothing_goes_to_the_bank_without_a_signature_and_evidence() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let pump = world.pump("akl", "a");
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    world.keys.broken.store(true, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert_eq!(pass.failed, 1);
    assert!(matches!(&world.state().submissions[&s].status, SubmissionStatus::Failed(reason) if reason.contains("HSM")));
    world.keys.broken.store(false, Ordering::SeqCst);
    world.vault.broken.store(true, Ordering::SeqCst);
    world.ok(
        &mia(),
        Command::Resubmit {
            run: "r1".to_string(),
            value_date: d(2027, 11, 15),
        },
    );
    let pass = pump.pass().await;
    assert_eq!(pass.failed, 1);
    assert!(matches!(&world.state().submissions["r1-2"].status, SubmissionStatus::Failed(reason) if reason.contains("vault")));
    assert_eq!(world.bank.lock().arrivals, 0);

    world.module.down.store(true, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert!(pass.problems[0].contains("could not be asked"));
    assert!(pump.drill("d9").await.is_err());
    let empty = World {
        module: Local::default(),
        ..world
    };
    let pass = empty.pump("akl", "a").pass().await;
    assert_eq!(pass.problems, vec!["the module has no configuration"]);
    assert!(empty.pump("akl", "a").drill("d9").await.unwrap_err().contains("no configuration"));
}

/// Purpose: secrets are read from the mounted directory by safe names only,
/// trimmed, and re-read each time; the log notifier accepts a notice; the
/// module is declared to the node host under its permanent name; the clock
/// the service uses is New Zealand's.
/// Why it matters: a credential name that could walk out of the secrets
/// directory would be a way to read any file in the pod, and a rotated
/// bank credential must take effect without a restart.
#[tokio::test]
async fn the_services_small_parts_do_what_they_say() {
    let dir = std::env::temp_dir().join(format!("ic-payments-secrets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bank-akl"), "s3cret\n").unwrap();
    std::fs::write(dir.join("empty"), "\n").unwrap();
    let secrets = MountedSecrets(dir.clone());
    assert_eq!(secrets.secret("bank-akl").as_deref(), Some("s3cret"));
    std::fs::write(dir.join("bank-akl"), "rotated").unwrap();
    assert_eq!(secrets.secret("bank-akl").as_deref(), Some("rotated"));
    assert_eq!(secrets.secret("empty"), None);
    assert_eq!(secrets.secret("missing"), None);
    assert_eq!(secrets.secret("../bank-akl"), None);
    assert_eq!(secrets.secret(""), None);
    std::fs::remove_dir_all(&dir).unwrap();

    LogNotifier.notify(Role::Treasury, "large_payment_pre_advice", "run r1").await.unwrap();
    let spec = ic_payments_service::module_spec().unwrap();
    assert_eq!(spec.name, ic_payments_service::MODULE);
    let utc = chrono::Utc::now().naive_utc();
    let ahead = (nz_now() - utc).num_minutes();
    assert!(
        (12 * 60 - 1..=13 * 60 + 1).contains(&ahead),
        "New Zealand is 12 or 13 hours ahead of UTC, not {ahead} minutes"
    );
    let other = ic_core::Command::OpenBusinessDate { date: d(2027, 1, 1) };
    assert!(policy_of(&other).is_none());
}
