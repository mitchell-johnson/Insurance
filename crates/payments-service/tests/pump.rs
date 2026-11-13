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
    /// What other modules were told, and what they will answer.
    told: Arc<Mutex<Vec<(String, Value)>>>,
    other_answer: Arc<Mutex<Option<Value>>>,
    /// Questions it will still answer; unset, it answers them all.
    answers_left: Arc<Mutex<Option<u32>>>,
    /// Answers every question with something that is not an answer.
    garbled: Arc<AtomicBool>,
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
    async fn tell(&self, module: &str, command: Value) -> Result<Value, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("no leader".to_string());
        }
        self.told.lock().unwrap().push((module.to_string(), command));
        Ok(self
            .other_answer
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(serde_json::json!({"index": 1, "result": {"Ok": null}})))
    }
    async fn query(&self, question: Value) -> Result<Value, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("no leader".to_string());
        }
        if self.garbled.load(Ordering::SeqCst) {
            return Ok(serde_json::json!("try again later"));
        }
        if let Some(left) = self.answers_left.lock().unwrap().as_mut() {
            if *left == 0 {
                return Err("no leader".to_string());
            }
            *left -= 1;
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
    async fn send_to(&self, _policy: &PolicyId, key: &str, caller: &str, command: ic_core::Command) -> Result<Option<String>, String> {
        if self.down.load(Ordering::SeqCst) {
            return Err("the core cannot be reached".to_string());
        }
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
        let mut secrets: BTreeMap<String, String> = REGIONS.iter().map(|r| (format!("bank-{r}"), format!("token-{r}"))).collect();
        for r in REGIONS {
            secrets.insert(format!("cert-{r}"), format!("{CLIENT_CERT}{CLIENT_KEY}"));
        }
        secrets.insert("bank-ca".to_string(), TEST_CA.to_string());
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
            holds: None,
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
        life: None,
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

    // A pump in a region the configuration does not know has no bank to
    // ask, so it says so and decides nothing.
    world.bank.lock().down = false;
    let pass = world.pump("nowhere", "z").pass().await;
    assert!(pass.problems.len() >= 2, "{:?}", pass.problems);
    assert_eq!((pass.sent, pass.failed, pass.messages), (0, 0, 0));
    assert_eq!(world.state().submissions[&unsure].status, SubmissionStatus::Sending);
}

/// Purpose: an instruction for the claims module, which does not itself
/// name a policy, is carried to the core under the policy it is held for;
/// when the core refuses it the refusal is recorded against the instruction
/// and a person is given the exception.
/// Why it matters: this is how claims hears that a bank refused a benefit.
/// A pump that could not route it, or dropped a refusal, would leave the
/// payment showing as with the bank for ever.
#[tokio::test]
async fn a_notice_for_claims_is_carried_to_the_policys_partition() {
    use ic_payments::model::{About, Instruction, OutboxEntry};
    let world = World::new().await;
    let pump = world.pump("akl", "a");
    {
        let mut guard = world.module.state.lock().unwrap();
        let state = &mut guard.0;
        let entry = OutboxEntry {
            key: "claim-payment-failed-pay-a".to_string(),
            instruction: Instruction::CoreAt {
                policy: world.policy.clone(),
                caller: "payments".to_string(),
                command: Box::new(ic_core::Command::Claims(Box::new(ic_core::claims_module::Command::PaymentReturned {
                    claim: "clm-nobody".to_string(),
                    payment_ref: "pay-a".to_string(),
                }))),
            },
            about: Some(About::Item("pay-a".to_string())),
        };
        state.outbox.insert(state.next_outbox, entry);
        state.next_outbox += 1;
    }
    let pass = pump.pass().await;
    assert_eq!(pass.instructions, 1, "{:?}", pass.problems);
    let state = world.state();
    assert!(state.outbox.is_empty());
    // The core has no such claim, and says so; nobody is left guessing.
    assert!(
        state.exceptions.values().any(|e| e.kind == ExceptionKind::CoreRefused),
        "{:?}",
        state.exceptions
    );
}

/// Purpose: a message for another module (a reinsurer settlement that was
/// paid) is delivered to that module by name; its refusal is recorded and
/// given to a person; while the cluster cannot be reached it waits.
/// Why it matters: the reinsurance statement stays "instructed" and the
/// finance close item stays open until this arrives. A lost message is a
/// close that cannot be signed; a dropped refusal is one nobody chases.
#[tokio::test]
async fn a_message_for_another_module_is_delivered_or_its_refusal_kept() {
    use ic_payments::model::{Instruction, OutboxEntry};
    let world = World::new().await;
    let pump = world.pump("akl", "a");
    let message = |n: u32| {
        serde_json::json!({"key": format!("payments:settled:ri-stmt-{n}"), "caller": "payments", "on": "2027-11-15",
        "command": {"record_settlement": {"number": n, "payment_ref": format!("ri-stmt-{n}"), "amount_minor": 5}}})
    };
    let queue = |n: u32| {
        let mut guard = world.module.state.lock().unwrap();
        let state = &mut guard.0;
        let entry = OutboxEntry {
            key: format!("reinsurance-settled-ri-stmt-{n}"),
            instruction: Instruction::Module {
                module: "reinsurance".to_string(),
                command: message(n),
            },
            about: Some(ic_payments::model::About::Item(format!("ri-stmt-{n}"))),
        };
        state.outbox.insert(state.next_outbox, entry);
        state.next_outbox += 1;
    };
    queue(7);
    world.module.down.store(true, Ordering::SeqCst);
    assert!(!pump.pass().await.problems.is_empty());
    assert!(world.module.told.lock().unwrap().is_empty());
    world.module.down.store(false, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert_eq!((pass.instructions, pass.problems.len()), (1, 0), "{:?}", pass.problems);
    assert_eq!(*world.module.told.lock().unwrap(), vec![("reinsurance".to_string(), message(7))]);
    assert!(world.state().outbox.is_empty());
    assert!(world.state().exceptions.is_empty());

    queue(8);
    *world.module.other_answer.lock().unwrap() = Some(serde_json::json!({"index": 9, "result": {"Err": {"code": "unknown_statement", "message": "no"}}}));
    pump.pass().await;
    let state = world.state();
    assert!(state.outbox.is_empty());
    assert!(
        state.exceptions.values().any(|e| e.kind == ExceptionKind::CoreRefused),
        "{:?}",
        state.exceptions
    );

    use ic_payments_service::ports::refusal;
    assert_eq!(refusal(&serde_json::json!({"Err": {"message": "no code"}})).as_deref(), Some("refused"));
    assert_eq!(refusal(&serde_json::json!({"Ok": []})), None);
}

/// Purpose: when the module stops answering part way through a pass, or
/// answers with something unreadable, the pump reports each step it could
/// not do, sends nothing, and the next pass does the work.
/// Why it matters: leadership moves while pumps run. A pump that acted on
/// half an answer, or that died on one, would send without a claim or stop
/// paying until someone restarted it.
#[tokio::test]
async fn a_module_that_falters_mid_pass_costs_nothing() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let pump = world.pump("akl", "a");
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);

    // It answers the first question (its configuration) and no more.
    *world.module.answers_left.lock().unwrap() = Some(1);
    let pass = pump.pass().await;
    assert_eq!(pass.sent, 0);
    assert_eq!(pass.problems.iter().filter(|p| p.contains("no leader")).count(), 3, "{:?}", pass.problems);
    assert_eq!(world.bank.lock().arrivals, 0);
    *world.module.answers_left.lock().unwrap() = None;

    world.module.garbled.store(true, Ordering::SeqCst);
    let pass = pump.pass().await;
    assert!(pass.problems[0].contains("unreadable answer"), "{:?}", pass.problems);
    assert_eq!(world.bank.lock().arrivals, 0);
    world.module.garbled.store(false, Ordering::SeqCst);

    let pass = pump.pass().await;
    assert_eq!((pass.sent, pass.problems.len()), (1, 0), "{:?}", pass.problems);
    assert_eq!(world.state().submissions[&s].status, SubmissionStatus::Sent);
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
        format: Default::default(),
        url: endpoint.url.trim_end_matches('/').to_string(),
        token: "token-akl".to_string(),
        accounts: Default::default(),
        identity: None,
        authority: None,
    };
    let batch = world.state().batch(&s2).unwrap();
    let manifest = batch.manifest("test".to_string(), "k".to_string(), "sig".to_string()).unwrap();
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
    let good = batch.manifest("test".to_string(), "k".to_string(), "sig".to_string()).unwrap();
    let file = |token: &str| Target {
        kind: ic_payments::config::ChannelKind::File,
        format: Default::default(),
        url: world.bank.url.clone(),
        token: token.to_string(),
        accounts: Default::default(),
        identity: None,
        authority: None,
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
    assert_eq!(bank.holds(&file("token-akl"), "r1-1").await.unwrap(), Some(batch.content_hash().unwrap()));
    assert_eq!(bank.holds(&api, "r9-9").await.unwrap(), None);
    let mut other = batch.clone();
    other.items[0].amount_minor = 999_00;
    let manifest = other.manifest("test".to_string(), "k".to_string(), "sig".to_string()).unwrap();
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
    let policy: PolicyId = serde_json::from_value(serde_json::json!("pol-1")).unwrap();
    let release = ic_core::Command::ReleaseRefundPayable {
        policy: policy.clone(),
        refund: "rf1".to_string(),
    };
    assert_eq!(policy_of(&release), Some(&policy));

    // A bank that answers with something that is not an answer: the outcome
    // is unknown, which is never treated as "not sent".
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 9\r\nconnection: close\r\n\r\nnot json!")
                .await;
        }
    });
    let to = Target {
        kind: ic_payments::config::ChannelKind::File,
        format: Default::default(),
        url,
        token: "t".to_string(),
        accounts: Default::default(),
        identity: None,
        authority: None,
    };
    assert!(matches!(HttpBank::default().messages(&to, 0).await, Err(ChannelError::Unknown(_))));
}

/// Purpose: the pump's settings name what is missing, refuse a grace period
/// short enough to be dangerous, and have no fallback to an unsigned or
/// unrecorded submission; the binary started with nothing says what it needs.
/// Why it matters: a pump that started on defaults could send a bank a file
/// with no real signature and no evidence, or declare lost a file another
/// pump was still sending.
#[test]
fn the_pump_does_not_start_half_configured() {
    use ic_payments_service::settings::PumpSettings;
    let full: BTreeMap<&str, &str> = BTreeMap::from([
        ("IC_NODES", "node-1:7000,node-2:7000"),
        ("IC_CLUSTER_TOKEN", "test-cluster-token-0123456789abcdef"),
        ("IC_REGION", "syd"),
        ("HOSTNAME", "pump-0"),
        ("IC_BANK_SECRETS_DIR", "/mnt/bank"),
        ("IC_KEY_VAULT_URL", "https://kv.vault.azure.net"),
        ("IC_BLOB_ENDPOINT", "https://st.blob.core.windows.net"),
    ]);
    let load = |env: &BTreeMap<&str, &str>| PumpSettings::from_lookup(|key| env.get(key).map(|v| v.to_string()));
    let settings = load(&full).unwrap();
    assert_eq!(
        (settings.region.as_str(), settings.instance.as_str(), settings.container.as_str()),
        ("syd", "pump-0", "payments-evidence")
    );
    assert_eq!((settings.nodes.len(), settings.interval.as_secs(), settings.grace.num_minutes()), (2, 15, 10));
    for key in [
        "IC_NODES",
        "IC_REGION",
        "HOSTNAME",
        "IC_BANK_SECRETS_DIR",
        "IC_KEY_VAULT_URL",
        "IC_BLOB_ENDPOINT",
        "IC_CLUSTER_TOKEN",
    ] {
        let mut env = full.clone();
        env.remove(key);
        assert!(load(&env).is_err(), "started without {key}");
    }
    let with = |key: &'static str, value: &'static str| {
        let mut env = full.clone();
        env.insert(key, value);
        load(&env)
    };
    assert!(with("IC_PUMP_GRACE_MINUTES", "1").unwrap_err().contains("at least 2"));
    assert!(with("IC_PUMP_INTERVAL_SECS", "0").is_err());
    assert!(with("IC_NODES", " , ").unwrap_err().contains("IC_NODES"));
    assert!(with("IC_PUMP_INTERVAL_SECS", "soon").unwrap_err().contains("IC_PUMP_INTERVAL_SECS"));
    assert_eq!(with("IC_PAYMENTS_INSTANCE", "x").unwrap().instance, "x");
    assert_eq!(with("IC_PAYMENTS_CONTAINER", "c").unwrap().container, "c");
    // The customer master is both an address and a scope, or not there.
    assert!(with("IC_CUSTOMERS_URL", "https://customers").unwrap_err().contains("together"));
    assert!(settings.customers.is_none());
    let mut env = full.clone();
    env.extend([("IC_CUSTOMERS_URL", "https://customers"), ("IC_CUSTOMERS_SCOPE", "api://ic-customers/.default")]);
    let asking = load(&env).unwrap();
    let pump = ic_payments_service::pump::DeployedPump::deployed(&asking, ic_azure::credential::AzureCredential::static_token("t"));
    assert!(pump.holds.is_some());

    // Built as deployed, the pump is in the region and under the name given.
    let pump = ic_payments_service::pump::DeployedPump::deployed(&settings, ic_azure::credential::AzureCredential::static_token("t"));
    assert_eq!((pump.region.as_str(), pump.instance.as_str(), pump.grace.num_minutes()), ("syd", "pump-0", 10));

    // The coverage tooling's own variables are kept so the run is measured.
    let measured = std::env::vars().filter(|(key, _)| key.starts_with("LLVM_"));
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ic-payments-pump"))
        .env_clear()
        .envs(measured)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("IC_CLUSTER"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Purpose: the binary's three modes do what they say: `once` fails when a
/// pass left something undone, `drill` fails when a channel could not be
/// reached, the standing pump keeps passing until it is told to stop, and
/// anything else is a usage error.
/// Why it matters: the drill's exit status is what an evacuation exercise
/// and the scheduled job read. A drill that failed and exited zero would
/// certify a region that cannot pay anyone.
#[tokio::test]
async fn the_binarys_modes_report_failure_as_failure() {
    let mut world = World::new().await;
    world.payee("p1", PayeeKind::Customer);
    let args = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    let quick = std::time::Duration::from_millis(5);
    let never = std::future::pending::<()>;
    let sydney = world.pump("syd", "s");
    sydney.run(&args(&["once"]), quick, never()).await.unwrap();
    sydney.run(&args(&["drill", "d1"]), quick, never()).await.unwrap();
    // The scheduled drill names itself, so a retry the same day is the same drill.
    sydney.run(&args(&["drill"]), quick, never()).await.unwrap();
    sydney.run(&args(&["drill"]), quick, never()).await.unwrap();
    let named = world.module.state.lock().unwrap().0.drills.keys().filter(|id| id.ends_with("-syd")).count();
    assert_eq!(named, 1);
    let melbourne = world.pump("mel", "m");
    assert!(melbourne
        .run(&args(&["drill", "d2"]), quick, never())
        .await
        .unwrap_err()
        .contains("could not be reached"));
    assert!(sydney.run(&args(&["twice"]), quick, never()).await.unwrap_err().contains("usage"));

    // The standing pump sends what is queued, reports what it cannot do,
    // and stops when told.
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    let state = world.module.state.clone();
    let sent = async move {
        while state.lock().unwrap().0.submissions[&s].status != SubmissionStatus::Sent {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    sydney.run(&[], quick, sent).await.unwrap();
    world.bank.lock().down = true;
    assert!(sydney.run(&args(&["once"]), quick, never()).await.unwrap_err().contains("channel anz-file"));
    sydney.run(&[], quick, tokio::time::sleep(std::time::Duration::from_millis(20))).await.unwrap();
}

/// Purpose: a pump started afresh does not read again the bank messages an
/// earlier pump recorded; but a message the module did not take holds the
/// recorded place before it, so a fresh pump is offered it again, and what
/// came after it is recorded once only.
/// Why it matters: a pump restarts on every deployment. Reading a bank's
/// whole history each time gets slower every day the system runs; skipping
/// a message the module never took would lose a settlement or a return for
/// good, which is money moved that the ledger never hears about.
#[tokio::test]
async fn a_restarted_pump_carries_on_where_the_last_one_stopped() {
    let world = World::new().await;
    let statement = |day: u32, opening: i64| BankMessage::Statement {
        account: "anz-sf1-collection".to_string(),
        date: d(2027, 11, day),
        opening_minor: opening,
        closing_minor: opening,
        lines: vec![],
    };
    let read = |world: &World| world.state().messages_read.values().copied().max().unwrap_or(0);
    world.bank.push(statement(15, 1_000_000_00));
    let first = world.pump("akl", "a").pass().await;
    assert_eq!(first.messages, 1, "{:?}", first.problems);
    assert_eq!(read(&world), 1);
    assert_eq!(world.pump("akl", "b").pass().await, Default::default(), "nothing is read twice");

    world.bank.push(BankMessage::Settlement {
        id: "never-sent".to_string(),
        settled_on: d(2027, 11, 16),
    });
    world.bank.push(statement(16, 1_000_000_00));
    let second = world.pump("akl", "c").pass().await;
    assert_eq!(second.messages, 1, "{:?}", second.problems);
    assert_eq!(read(&world), 1, "the place stops before the message the module did not take");
    assert_eq!(world.state().books["anz-sf1-collection"].statement_date, Some(d(2027, 11, 16)));
    let exceptions = world.state().exceptions.len();

    let third = world.pump("akl", "d").pass().await;
    assert!(third.problems.iter().any(|p| p.contains("message 2")), "{:?}", third.problems);
    assert_eq!(third.messages, 1, "the later statement is offered again and answered as before");
    assert_eq!(world.state().exceptions.len(), exceptions, "and recorded once");
}

/// Purpose: with the first bank's channels switched to ISO 20022, a run
/// goes as a `pain.001` file and an urgent run as an ISO 20022 request; the
/// bank checks each, the evidence vault holds the XML, and the hash the
/// module records is the hash of the XML the bank holds; the test bank
/// refuses a request whose message ID is not its idempotency key or whose
/// control sum does not match; and a drill under an ID too long for the
/// format fails its probe, saying so, with nothing sent.
/// Why it matters: switching a bank's format is a configuration change on
/// a live system. Nothing about exactly-once sending, evidence or the
/// drill may depend on which format a channel speaks.
#[tokio::test]
async fn a_bank_that_speaks_iso_20022_is_sent_iso_20022() {
    let mut world = iso_world().await;
    world.payee("p1", PayeeKind::Beneficiary);
    let pump = world.pump("akl", "a");

    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    let u = world.run("u1", RunType::LumpSumClaims, true, vec![pay("i2", "p1", 9_000_00)]);
    let pass = pump.pass().await;
    assert_eq!((pass.sent, pass.problems.len()), (2, 0), "{:?}", pass.problems);
    let state = world.state();
    assert_eq!(state.submissions[&u].status, SubmissionStatus::Acknowledged);
    let xml = world
        .vault
        .get(&format!("payments/{s}/file.xml"))
        .await
        .unwrap()
        .expect("the XML is the evidence");
    let xml = String::from_utf8(xml).unwrap();
    assert!(xml.contains("<EndToEndId>i1</EndToEndId>"));
    let held = world.bank.lock().held[&s].clone();
    assert_eq!(held.items, vec![("i1".to_string(), 300_00)]);
    assert_eq!(state.submissions[&s].content_hash.as_deref(), Some(held.sha256.as_str()));
    assert_eq!(world.api.lock().held[&u].items, vec![("i2".to_string(), 9_000_00)]);

    // The test bank checks what a bank checks.
    let batch = state.batch(&u).unwrap();
    let post = |key: &str, body: String| {
        reqwest::Client::new()
            .post(format!("{}/batches", world.api.url))
            .bearer_auth("token-akl")
            .header("idempotency-key", key.to_string())
            .header("content-type", "application/xml")
            .body(body)
            .send()
    };
    let wrong_key = post("u1-9", batch.wire().unwrap()).await.unwrap();
    assert_eq!(wrong_key.status(), 422);
    assert!(wrong_key.text().await.unwrap().contains("message ID"));
    let wrong_sum = batch.wire().unwrap().replacen("<CtrlSum>9000.00</CtrlSum>", "<CtrlSum>9.00</CtrlSum>", 1);
    let refused = post(&u, wrong_sum).await.unwrap();
    assert!(refused.text().await.unwrap().contains("control sum"));

    // A drill whose ID the format cannot carry sends nothing and says why.
    let long = "d".repeat(30);
    assert!(!pump.drill(&long).await.unwrap());
    let probes = world.state().drills[&long].probes.clone();
    let file = &probes["anz-file"];
    assert!(!file.ok && file.detail.contains("ISO 20022 identifier"), "{file:?}");
    assert!(file.evidence_hash.starts_with("unwritten: "));
    assert!(probes["bnz-file"].ok, "the other bank still speaks the system's own format");
    assert_eq!(world.bank.lock().held.len(), 1);
}

const TEST_CA: &str = include_str!("tls/ca.pem");
const CLIENT_CERT: &str = include_str!("tls/client.pem");
const CLIENT_KEY: &str = include_str!("tls/client.key");

/// A fake bank that takes only callers with a client certificate from the
/// test authority, over HTTPS.
async fn tls_bank() -> FakeBank {
    let tls = ic_payments_service::fake::BankTls {
        certificate: include_str!("tls/server.pem"),
        key: include_str!("tls/server.key"),
        client_authority: TEST_CA,
    };
    FakeBank::start_tls(&["token-akl", "token-syd"], tls).await.unwrap()
}

/// The test world with the first bank's channels switched to ISO 20022, and
/// that bank's fakes speaking it, numbering each account as configured. Its
/// file channel is reached over mutual TLS, as a real bank's would be.
async fn iso_world() -> World {
    let mut world = World::new().await;
    world.bank = tls_bank().await;
    let mut config: ic_payments::Config = serde_json::from_value(world.module.query(serde_json::json!({"q": "config"})).await.unwrap()).unwrap();
    let iso = common::iso_config();
    for (id, channel) in &mut config.channels {
        channel.format = iso.channels[id].format;
        for (region, endpoint) in &mut channel.endpoints {
            endpoint.client_certificate = iso.channels[id].endpoints[region].client_certificate.clone();
            endpoint.bank_authority = iso.channels[id].endpoints[region].bank_authority.clone();
            if id == "anz-file" {
                endpoint.url = format!("{}/", world.bank.url);
            }
        }
    }
    for (code, account) in &mut config.accounts {
        account.at_bank = iso.accounts[code].at_bank.clone();
    }
    let numbers: BTreeMap<String, String> = config
        .accounts
        .iter()
        .filter_map(|(code, a)| Some((code.clone(), a.at_bank.as_ref()?.number.clone())))
        .collect();
    world.ok(
        &cal(),
        Command::ActivateConfig {
            version: 2,
            config: Box::new(config),
        },
    );
    for bank in [&world.bank, &world.api] {
        let mut inner = bank.lock();
        inner.iso = true;
        inner.account_numbers = numbers.clone();
        inner.account_numbers.insert("settlement".to_string(), numbers["anz-sf1-claims"].clone());
    }
    world
}

/// Purpose: a bank that speaks ISO 20022 is understood: its `pain.002`
/// rejects an item, its `camt.054` settles the file and returns an item, its
/// `camt.053` statement reconciles; a document holding several messages is
/// recorded part by part, and the place in the bank's sequence stops before
/// a document any part of which the module did not take, so a restarted
/// pump is offered it again whole; a document that cannot be read at all is
/// reported each pass and nothing after it is read.
/// Why it matters: these are the bank's answers about real money. One
/// lost, or one read past, is a payment the ledger never hears about.
#[tokio::test]
async fn a_bank_that_speaks_iso_20022_is_understood() {
    let mut world = iso_world().await;
    world.payee("p1", PayeeKind::Beneficiary);
    let pump = world.pump("akl", "a");
    world.bank.lock().reject_items.insert("i2".to_string(), "AC01".to_string());
    let s = world.run(
        "r1",
        RunType::BenefitPayments,
        false,
        vec![pay("i1", "p1", 300_00), pay("i2", "p1", 50_00), pay("i3", "p1", 20_00)],
    );
    assert_eq!(pump.pass().await.sent, 1);
    world.bank.acknowledge(&s);
    world.bank.settle(&s, d(2027, 11, 16));
    world.bank.push(BankMessage::Return {
        item: "i3".to_string(),
        reason: ReturnReason::AccountClosed,
        returned_on: d(2027, 11, 17),
    });
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.problems.len()), (3, 0), "{:?}", pass.problems);
    let state = world.state();
    assert_eq!(state.submissions[&s].settled_on, Some(d(2027, 11, 16)));
    assert_eq!(state.control_totals("r1").unwrap()["rejected"], 50_00);

    // A statement in camt.053 is reconciled against what was expected.
    world.bank.push(BankMessage::Statement {
        account: "anz-sf1-claims".to_string(),
        date: d(2027, 11, 17),
        opening_minor: 1_000_000_00,
        closing_minor: 1_000_000_00 - 320_00 + 20_00,
        lines: vec![
            StatementLine {
                id: "l1".to_string(),
                amount_minor: -320_00,
                reference: s.clone(),
                payer_account: None,
            },
            StatementLine {
                id: "l2".to_string(),
                amount_minor: 20_00,
                reference: "i3".to_string(),
                payer_account: None,
            },
        ],
    });
    let pass = pump.pass().await;
    assert_eq!((pass.messages, pass.problems.len()), (1, 0), "{:?}", pass.problems);
    assert_eq!(world.state().books["anz-sf1-claims"].statement_date, Some(d(2027, 11, 17)));

    // One document, two parts: the first taken, the second refused. The
    // place stops before the document, so it is offered again whole.
    let number = world.bank.lock().account_numbers["settlement"].clone();
    let entry = |id: &str| {
        format!("<Ntry><Amt Ccy=\"NZD\">0.00</Amt><CdtDbtInd>DBIT</CdtDbtInd><BookgDt><Dt>2027-11-18</Dt></BookgDt><NtryDtls><Btch><PmtInfId>{id}</PmtInfId></Btch></NtryDtls></Ntry>")
    };
    let two = format!(
        "<?xml version=\"1.0\"?><Document><BkToCstmrDbtCdtNtfctn><Ntfctn><Acct><Id><Othr><Id>{number}</Id></Othr></Id></Acct>{}{}</Ntfctn></BkToCstmrDbtCdtNtfctn></Document>",
        entry(&s),
        entry("never-sent")
    );
    world.bank.push_document(&two);
    let before = world.state().messages_read["anz-file"];
    let pass = pump.pass().await;
    assert!(pass.problems.iter().any(|p| p.contains("unknown_submission")), "{:?}", pass.problems);
    assert_eq!(
        world.state().messages_read["anz-file"],
        before,
        "the place stops before the half-taken document"
    );
    let again = world.pump("akl", "b").pass().await;
    assert!(again.problems.iter().any(|p| p.contains("unknown_submission")), "{:?}", again.problems);

    // A document nobody can read stops the reading there, every pass, and
    // says why.
    let stuck = iso_world().await;
    stuck.bank.push_document("<not xml");
    stuck.bank.push(BankMessage::Settlement {
        id: "later".to_string(),
        settled_on: d(2027, 11, 18),
    });
    let reader = stuck.pump("akl", "c");
    for _ in 0..2 {
        let pass = reader.pass().await;
        assert_eq!(pass.messages, 0);
        assert!(pass.problems.iter().any(|p| p.contains("document 1 cannot be read")), "{:?}", pass.problems);
    }
    // Behind a readable document, an unreadable one waits for the next pass.
    let good = ic_payments_service::fake::iso_document(
        &BankMessage::Settlement {
            id: "r9-1".to_string(),
            settled_on: d(2027, 11, 18),
        },
        &BTreeMap::new(),
    );
    let documents = vec![
        ic_payments_service::bank::Document { seq: 1, document: good },
        ic_payments_service::bank::Document {
            seq: 2,
            document: "<".to_string(),
        },
    ];
    let accounts = BTreeMap::from([("settlement".to_string(), "anz-sf1-claims".to_string())]);
    let read = ic_payments_service::bank::read_documents(documents, &accounts).unwrap();
    assert_eq!(read.iter().map(|n| (n.seq, n.part)).collect::<Vec<_>>(), vec![(1, 0)]);
}

/// Purpose: a bank that requires mutual TLS takes a caller presenting the
/// client certificate its authority issued and trusting the bank's private
/// authority; it turns away a caller with no certificate, with one it never
/// issued, or one that does not trust the bank, and in each case the pump
/// knows nothing was sent; a certificate or authority that is not valid PEM
/// is reported as such; a pump whose region has no certificate in its vault
/// says so and sends nothing.
/// Why it matters: a bank's host-to-host service identifies the insurer by
/// its certificate. Knowing for certain that a refused handshake sent
/// nothing is what lets the same file go again safely, and a missing
/// certificate must stop the channel loudly, not fall back to sending
/// without one.
#[tokio::test]
async fn a_bank_that_requires_mutual_tls_takes_only_its_own_certificates() {
    let mut world = iso_world().await;
    world.payee("p1", PayeeKind::Beneficiary);
    let s = world.run("r1", RunType::BenefitPayments, false, vec![pay("i1", "p1", 300_00)]);
    let batch = world.state().batch(&s).unwrap();
    let manifest = batch.manifest("test".to_string(), "k".to_string(), "sig".to_string()).unwrap();
    let ours = Target {
        kind: ic_payments::config::ChannelKind::File,
        format: ic_payments::config::WireFormat::Iso20022,
        url: world.bank.url.clone(),
        token: "token-akl".to_string(),
        accounts: Default::default(),
        identity: Some(format!("{CLIENT_CERT}{CLIENT_KEY}")),
        authority: Some(TEST_CA.to_string()),
    };
    let bank = HttpBank::default();
    let stranger = format!("{}{}", include_str!("tls/stranger.pem"), include_str!("tls/stranger.key"));
    // Not trusting the bank is found before anything is sent. A bank that
    // turns our certificate away does so, under TLS 1.3, after the request
    // may have left; the caller cannot know, and says so. The pump asks the
    // bank what it holds before it takes a submission, so with a refused
    // certificate it never takes one.
    let distrust = bank
        .submit(
            &Target {
                authority: None,
                ..ours.clone()
            },
            &batch,
            &manifest,
        )
        .await;
    assert!(matches!(distrust, Err(ChannelError::NotSent(_))), "{distrust:?}");
    for (who, to) in [
        (
            "no certificate",
            Target {
                identity: None,
                ..ours.clone()
            },
        ),
        (
            "a certificate the bank never issued",
            Target {
                identity: Some(stranger.clone()),
                ..ours.clone()
            },
        ),
    ] {
        let sent = bank.submit(&to, &batch, &manifest).await;
        assert!(matches!(sent, Err(ChannelError::NotSent(_) | ChannelError::Unknown(_))), "{who}: {sent:?}");
        assert!(bank.holds(&to, &s).await.is_err(), "{who}");
    }
    let mut impostor = world.pump("akl", "i");
    impostor.secrets.insert("cert-akl".to_string(), stranger);
    let pass = impostor.pass().await;
    assert_eq!(pass.sent, 0, "{:?}", pass.problems);
    assert_eq!(
        world.state().submissions[&s].status,
        SubmissionStatus::Queued,
        "a refused certificate takes nothing"
    );
    assert!(world.bank.lock().held.is_empty());
    let receipt = bank.submit(&ours, &batch, &manifest).await.unwrap();
    assert!(receipt.reference.starts_with("BANK-"));
    assert_eq!(bank.holds(&ours, &s).await.unwrap(), batch.content_hash().ok());
    assert_eq!(bank.messages(&ours, 0).await.unwrap(), vec![]);

    let garbled = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";
    for (to, what) in [
        (
            Target {
                identity: Some("not a certificate".to_string()),
                ..ours.clone()
            },
            "client certificate cannot be used",
        ),
        (
            Target {
                authority: Some("not a certificate".to_string()),
                ..ours.clone()
            },
            "certificate authority cannot be used",
        ),
        (
            Target {
                identity: Some(garbled.to_string()),
                ..ours.clone()
            },
            "certificates cannot be used",
        ),
        (
            Target {
                authority: Some(garbled.to_string()),
                ..ours.clone()
            },
            "certificates cannot be used",
        ),
    ] {
        let refused = bank.holds(&to, &s).await.unwrap_err();
        assert!(matches!(&refused, ChannelError::NotSent(e) if e.contains(what)), "{what}: {refused:?}");
    }

    // A pump whose region's vault lacks the certificate cannot reach the
    // bank, and says which secret is missing.
    let s2 = world.run("r2", RunType::BenefitPayments, false, vec![pay("i2", "p1", 400_00)]);
    let mut uncertified = world.pump("akl", "u");
    uncertified.secrets.remove("cert-akl");
    let pass = uncertified.pass().await;
    assert!(
        pass.problems.iter().any(|p| p.contains("client certificate cert-akl is not available")),
        "{:?}",
        pass.problems
    );
    let mut untrusting = world.pump("akl", "v");
    untrusting.secrets.remove("bank-ca");
    let pass = untrusting.pass().await;
    assert!(
        pass.problems.iter().any(|p| p.contains("authority bank-ca is not available")),
        "{:?}",
        pass.problems
    );
    assert_eq!(world.state().submissions[&s2].status, SubmissionStatus::Queued);
    // With it, the pump sends over mutual TLS.
    let pass = world.pump("akl", "w").pass().await;
    assert_eq!((pass.sent, pass.problems.len()), (2, 0), "{:?}", pass.problems);
}

/// Purpose: the pump asks the customer master about the party a refund is
/// owed to, records the answer, and does not ask again until the record is
/// half as old as the check allows; a hold the master reports stops the
/// refund joining a run, and its end lets it through.
/// Why it matters: the customer master owns a person's own hold on money
/// going out (decision 3); payments must hear of it before the money is
/// put in a run, and hear of its end, without asking on every pass.
#[tokio::test]
async fn the_pump_hears_a_customers_own_hold_from_the_customer_master() {
    use ic_payments_service::fake::FakeMaster;
    use ic_payments_service::ports::MasterHolds;
    let mut world = World::new().await;
    let master = FakeMaster::start(&[("payer", true)]).await.unwrap();
    let mut pump = world.pump("akl", "a");
    pump.holds = Some(Arc::new(MasterHolds {
        base: format!("{}/", master.url),
        scope: "api://ic-customers/.default".to_string(),
        credential: ic_azure::credential::AzureCredential::static_token("t"),
        http: reqwest::Client::new(),
    }));
    let policy = world.policy.clone();
    let create = Command::CreateAuthority {
        authority: "a1".to_string(),
        policy: policy.clone(),
        party: "payer".to_string(),
        life: None,
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
    // One pass raises the refund and then asks about its payer; the next
    // does not ask again.
    let pass = pump.pass().await;
    assert_eq!((pass.instructions, pass.holds, pass.problems.len()), (1, 1, 0), "{:?}", pass.problems);
    assert!(world.state().held("payer"));
    assert_eq!(pump.pass().await.holds, 0);
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
    let today = world.now.lock().unwrap().date();
    let prepare = Command::PrepareRun {
        run: "r1".to_string(),
        run_type: RunType::PremiumRefunds,
        fund: "sf1".to_string(),
        currency: NZD,
        value_date: today,
        urgent: false,
    };
    world.ok(&mia(), prepare);
    let add = Command::AddRunItems {
        run: "r1".to_string(),
        items: vec![item.clone()],
    };
    assert_eq!(world.send(&mia(), add.clone()).unwrap_err().code, "customer_hold");

    // The person lifts it at the master. Within fifteen minutes nobody asks.
    master.holds.lock().unwrap().insert("payer".to_string(), false);
    world.later(10);
    assert_eq!(pump.pass().await.holds, 0);
    world.later(6);
    assert_eq!(pump.pass().await.holds, 1);
    assert!(!world.state().held("payer"));
    world.ok(&mia(), add);
    // The party is still watched while the refund is in an unapproved run;
    // a master that has lost the party is a problem, not an answer.
    master.holds.lock().unwrap().clear();
    world.later(16);
    let pass = pump.pass().await;
    assert_eq!(pass.holds, 0);
    assert!(
        pass.problems.iter().any(|p| p.contains("hold for payer") && p.contains("404")),
        "{:?}",
        pass.problems
    );
}

/// Purpose: the customer master's answer is read strictly, a party ID that
/// cannot be one is never sent, and the refresh interval follows the
/// check's limit.
/// Why it matters: a misread answer would let money out under a hold, or
/// hold it for no reason; neither may happen quietly.
#[tokio::test]
async fn the_customer_masters_answer_is_read_strictly() {
    use ic_payments_service::ports::{hold_in, Holds, MasterHolds};
    use ic_payments_service::pump::refresh_after;
    assert_eq!(hold_in(200, &serde_json::json!({"held": true})), Ok(true));
    assert!(hold_in(200, &serde_json::json!({"held": "yes"})).is_err());
    assert!(hold_in(403, &serde_json::json!({"held": false})).unwrap_err().contains("403"));
    let master = MasterHolds {
        base: "http://127.0.0.1:1".to_string(),
        scope: "s".to_string(),
        credential: ic_azure::credential::AzureCredential::static_token("t"),
        http: reqwest::Client::new(),
    };
    assert!(master.held("a/../b").await.unwrap_err().contains("not a party ID"));
    assert!(master.held("p1").await.is_err());
    let answers: BTreeMap<String, bool> = BTreeMap::from([("p1".to_string(), true)]);
    assert_eq!(answers.held("p1").await, Ok(true));
    assert!(answers.held("p2").await.is_err());
    assert_eq!(refresh_after(None), Duration::minutes(15));
    assert_eq!(refresh_after(Some(45)), Duration::minutes(23));
}
