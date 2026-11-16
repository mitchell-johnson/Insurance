//! Each AIA New Zealand product from quote to a paid claim, through the
//! modules as they run in production: the core quotes the application, the
//! underwriting module takes it from proposal to decision, the driver
//! carries it to reinsurance for capacity and to the core to issue on the
//! quoted product version, the core bills it month by month and takes the
//! customer's payments, and the claims module on the partition admits a
//! claim and releases the payment that the payments module settles
//! (docs/aia-products.md, "Each product end to end").

mod common;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use common::*;
use ic_core::claims_module as claims;
use ic_core::command::{CoverSpec, IssuePolicy, LifeSpec};
use ic_core::policy::LifeKind;
use ic_core::{accounts, Command as CoreCommand, GroupId, LifeFactors, LifeId, PartyId, PayableKind, PolicyId, PolicyStatus};
use ic_ledger::PartitionId;
use ic_log::StateMachine;
use ic_money::{Currency::NZD, Money};
use ic_product::Frequency;
use ic_reinsurance::capacity::Outcome;
use ic_reinsurance::driver::LocalModule;
use ic_reinsurance::loader::{load_rate_table, load_treaty};
use ic_sim::config::shipped_activations;
use ic_sim::Cluster;
use ic_underwriting::command::{Applied, Command, Effect, Envelope, OpenProposal, Reject};
use ic_underwriting::driver::{Channels, Core, CoreAnswer, Driver, DriverError, Forward, Pass, Refusal, Reinsurer, Schemes, Underwriting};
use ic_underwriting::model::ProposalStatus;
use ic_underwriting::progress::ProgressRow;
use ic_underwriting::special::{answer, Query};
use serde_json::Value;

use claims::command::{Notification, Periodic};
use claims::config::{Profile, Staff};
use claims::{Actor, AuthorityBook, Channel, ClaimType, DecisionKind, Flags, Line, Notifier, PayeeKind, Queue, Request, Role, Rules, RunKind, Status};

const RETAIL: &str = include_str!("../../../treaties/retail_life_surplus.star");
const GROUP: &str = include_str!("../../../treaties/group_life_surplus.star");
const RATES: &str = include_str!("../../../treaties/rates/ri_life_base.star");
const CLAIMS_BANK: &str = "nz-main-claims";

// ---- The modules in this process ------------------------------------------------------

#[derive(Clone)]
struct Module {
    world: Arc<Mutex<World>>,
}

impl Module {
    fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        f(&mut self.world.lock().unwrap())
    }
}

impl Underwriting for Module {
    async fn propose(&self, envelope: Envelope) -> Result<Result<Applied, Reject>, DriverError> {
        Ok(self.with(|w| {
            w.index += 1;
            w.log.push(envelope.clone());
            w.s.apply(w.index, envelope)
        }))
    }
    async fn ask(&self, query: Query) -> Result<Value, DriverError> {
        self.with(|w| answer(&w.s, serde_json::to_value(&query).unwrap())).map_err(DriverError::Answer)
    }
}

#[derive(Clone, Default)]
struct Reinsurance {
    module: LocalModule,
}

impl Reinsurer for Reinsurance {
    async fn propose(&self, envelope: ic_reinsurance::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        Ok(self.module.apply(envelope).result.map_err(|r| Refusal::new(&r.code, r.message)))
    }
    async fn capacity(&self, reservation: &str) -> Result<Option<Outcome>, DriverError> {
        Ok(self.module.state().capacity(reservation).map(|a| a.outcome))
    }
}

#[derive(Clone)]
struct Ledger {
    cluster: Arc<Mutex<Cluster>>,
}

impl Core for Ledger {
    async fn send(&self, _partition: PartitionId, key: &str, caller: &str, command: CoreCommand) -> Result<CoreAnswer, DriverError> {
        let applied = self.cluster.lock().unwrap().submit_keyed(0, key, caller, command);
        Ok(CoreAnswer {
            refused: applied.result.err().map(|r| Refusal::new(&r.code, r.message)),
            events: applied.events.into_iter().map(|e| e.event).collect(),
        })
    }
    async fn lapsed_for_underwriting(&self) -> Result<Vec<ic_core::policy::Policy>, DriverError> {
        Ok(vec![])
    }
    async fn policy(&self, _group: &ic_core::GroupId, policy: &ic_core::PolicyId) -> Result<Option<ic_core::policy::Policy>, DriverError> {
        Ok(self.cluster.lock().unwrap().state(0).policies.get(policy).cloned())
    }
}

#[derive(Clone, Default)]
struct Group;

impl Schemes for Group {
    async fn propose(&self, _envelope: ic_group::command::Envelope) -> Result<Result<(), Refusal>, DriverError> {
        Ok(Ok(()))
    }
}

#[derive(Clone)]
struct Portal {
    state: Arc<Mutex<ic_channels::state::ChannelState>>,
}

impl Channels for Portal {
    async fn publish(&self, key: &str, at: DateTime<Utc>, rows: &[ProgressRow]) -> Result<Result<(), Refusal>, DriverError> {
        let envelope: ic_channels::state::Envelope = serde_json::from_value(ic_underwriting::service::channels_envelope(key, at, rows)).unwrap();
        let output = self.state.lock().unwrap().apply(0, envelope);
        Ok(output.result.map(|_| ()).map_err(|r| Refusal::new(&r.code, r.message)))
    }
}

#[derive(Clone, Default)]
struct TakesAll;

impl Forward for TakesAll {
    async fn forward(&self, _seq: u64, _effect: &Effect) -> Result<Option<String>, DriverError> {
        Ok(None)
    }
}

fn start() -> NaiveDate {
    d(2027, 2, 1)
}

fn clock() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2027-02-01T09:00:00Z").unwrap().with_timezone(&Utc)
}

// ---- Claims configuration, as claims operations would activate it ---------------------

fn actor(who: &str) -> Actor {
    Actor {
        staff: who.to_string(),
        purpose: "working the claim".to_string(),
    }
}

fn rules() -> Rules {
    let kinds = [DecisionKind::Reserve, DecisionKind::Admit, DecisionKind::Decline, DecisionKind::EndBenefit];
    Rules {
        version: 1,
        escalation_days: [1, 3, 7],
        large_claim_minor: i64::MAX,
        two_signature_admission_minor: i64::MAX,
        two_signature_ex_gratia_minor: i64::MAX,
        two_signature_write_off_minor: i64::MAX,
        sharp_reserve_move_pct: 1_000,
        reason_codes: [("R-1".to_string(), kinds.into_iter().collect())].into(),
        straight_through_max_minor: [(ClaimType::Health, 20_000_00)].into(),
        // No health schedule of the insurer's own: the AIA products' limits are the ones applied.
        cause_codes: [("CC-0001".to_string(), vec!["cancer.skin".to_string()])].into(),
        medical_review_months: 6,
        income_declaration_months: 3,
        review_grace_days: 14,
        max_offset_pct: 25,
        late_death_days: 30,
        estate_simplified_max_minor: i64::MAX,
        pre_approval_days: 30,
        recurrence_days: 180,
        funeral_advance_max_minor: 15_000_00,
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
        ..Rules::default()
    }
}

fn authority() -> AuthorityBook {
    let types = [
        ClaimType::Death,
        ClaimType::TerminalIllness,
        ClaimType::Trauma,
        ClaimType::IncomeProtection,
        ClaimType::Health,
    ];
    let senior = Profile {
        lines: [Line::Health, Line::LumpSum, Line::IncomeProtection].into(),
        max_admission_minor: types.into_iter().map(|t| (t, i64::MAX)).collect(),
        max_payment_minor: i64::MAX,
        max_reserve_minor: i64::MAX,
        may_decline: true,
        max_ex_gratia_minor: 0,
        max_write_off_minor: 0,
        may_override_exclusion: false,
        may_reverse_admission: false,
        may_change_recovery_status: false,
        senior: true,
    };
    let person = |roles: &[Role]| Staff {
        profile: "senior".to_string(),
        roles: roles.iter().copied().collect(),
        queues: [
            Queue::Health,
            Queue::Group,
            Queue::LumpSum,
            Queue::IncomeProtectionNew,
            Queue::IncomeProtectionOngoing,
        ]
        .into(),
        wip_limit: 0,
        vulnerable_trained: true,
        related_parties: Default::default(),
    };
    AuthorityBook {
        version: 1,
        profiles: [("senior".to_string(), senior)].into(),
        staff: [
            ("sam".to_string(), person(&[Role::Assessor, Role::QueueLead])),
            ("sue".to_string(), person(&[Role::Assessor, Role::ClaimsManager])),
        ]
        .into(),
        prepared_by: "config-ann".into(),
        approved_by: vec!["config-bob".into(), "config-cat".into()],
    }
}

fn wrap(command: claims::Command) -> CoreCommand {
    CoreCommand::Claims(Box::new(command))
}

// ---- The bench ------------------------------------------------------------------------

struct Bench {
    module: Module,
    ledger: Ledger,
    driver: Driver<Module, Reinsurance, Ledger, Group, TakesAll, Portal>,
}

fn bench() -> Bench {
    let mut world = World::new();
    world.now = clock();

    let reinsurance = Reinsurance::default();
    let config = |command| {
        let applied = reinsurance.module.apply(ic_reinsurance::Envelope {
            key: format!("config-{}", reinsurance.module.state().state_hash()),
            caller: "config".into(),
            on: start(),
            command,
        });
        assert_eq!(applied.result, Ok(()));
    };
    for (file, source) in [("retail_life_surplus.star", RETAIL), ("group_life_surplus.star", GROUP)] {
        config(ic_reinsurance::Command::ActivateTreaty {
            treaty: Box::new(load_treaty(file, source).unwrap()),
        });
    }
    config(ic_reinsurance::Command::ActivateRateTable {
        table: Box::new(load_rate_table("ri_life_base.star", RATES).unwrap()),
    });

    let mut cluster = Cluster::in_memory(1, 7);
    cluster.keep_journal = true;
    cluster.open(start()).unwrap();
    for activation in shipped_activations().unwrap() {
        cluster.activate_everywhere(activation).unwrap();
    }
    cluster
        .activate_everywhere(wrap(claims::Command::ActivateRules { rules: Box::new(rules()) }))
        .unwrap();
    cluster
        .activate_everywhere(wrap(claims::Command::ActivateAuthority { book: Box::new(authority()) }))
        .unwrap();

    let module = Module {
        world: Arc::new(Mutex::new(world)),
    };
    let ledger = Ledger {
        cluster: Arc::new(Mutex::new(cluster)),
    };
    let portal = Portal {
        state: Arc::new(Mutex::new(ic_channels::state::ChannelState::shipped().unwrap())),
    };
    let driver = Driver {
        underwriting: module.clone(),
        reinsurer: reinsurance,
        core: ledger.clone(),
        schemes: Group,
        channels: portal,
        forward: TakesAll,
        batch: 10,
        clock,
    };
    Bench { module, ledger, driver }
}

/// One cover applied for: (cover code, cover type, sum insured or monthly benefit, option).
type Applying<'a> = (&'a str, &'a str, i64, Option<&'a str>);

/// What the applicant was quoted, and what the core then billed and was paid.
struct Issued {
    policy: PolicyId,
    quoted_minor: i64,
}

impl Bench {
    fn with_cluster<T>(&self, f: impl FnOnce(&mut Cluster) -> T) -> T {
        f(&mut self.ledger.cluster.lock().unwrap())
    }

    /// Quote, propose, disclose a clean history and let the driver carry the proposal to a policy.
    async fn apply(&self, id: &str, product: &str, covers: &[Applying<'_>], age: i32, class: Option<&str>) -> Issued {
        let (open, hash, quoted_minor) = self.quoted(id, product, covers, age, class);
        self.module
            .with(|w| w.send("new-business", Command::OpenProposal(Box::new(open))))
            .unwrap_or_else(|r| panic!("{product}: the proposal was refused: {}: {}", r.code, r.message));
        self.module.with(|w| w.disclose(id, "L1", &clean()));
        for _ in 0..3 {
            let pass: Pass = self.driver.deliver().await.unwrap();
            assert!(pass.parked.is_empty(), "{product}: {pass:?}");
        }
        let status = self.module.with(|w| w.p(id).status.clone());
        assert!(matches!(status, ProposalStatus::Issued { .. }), "{product}: {status:?}");

        let policy = self.module.with(|w| w.p(id).policy.clone());
        self.with_cluster(|c| {
            let issued = &c.state(0).policies[&policy];
            assert_eq!(
                (issued.status, issued.product_hash.as_str()),
                (PolicyStatus::InForce, hash.as_str()),
                "{product}"
            );
        });
        Issued { policy, quoted_minor }
    }

    /// An application quoted by the core as the screens ask for it: the proposal, the product
    /// version and the first month's premium.
    fn quoted(&self, id: &str, product: &str, covers: &[Applying<'_>], age: i32, class: Option<&str>) -> (OpenProposal, String, i64) {
        let today = start();
        let mut open = self.module.with(|w| {
            let specs: Vec<(&str, &str, &str, i64)> = covers.iter().map(|(code, kind, sum, _)| ("L1", *code, *kind, *sum)).collect();
            w.open_command(id, &["L1"], &specs, false)
        });
        open.product_code = product.into();
        open.group = GroupId::from(format!("grp-{id}").as_str());
        open.frequency = Frequency::Monthly;
        // Each named choice, and the first choice on every other axis of a cover's menu, or
        // the first excess level where the cover offers levels.
        let definition = self.with_cluster(|c| c.state(0).catalog.product_on(product, today).unwrap().clone());
        for (cover, (_, _, _, option)) in open.covers.iter_mut().zip(covers) {
            let spec = definition.cover(&cover.code).unwrap_or_else(|| panic!("{product} has no cover {}", cover.code));
            let named: Vec<(&str, &str)> = option.iter().flat_map(|o| o.split(',')).filter_map(|p| p.split_once('=')).collect();
            cover.option = if !spec.option_axes.is_empty() {
                let parts: Vec<(&str, &str)> = spec
                    .option_axes
                    .iter()
                    .map(|a| {
                        let chosen = named.iter().find(|(axis, _)| *axis == a.code).map(|(_, ch)| *ch);
                        (a.code.as_str(), chosen.unwrap_or(a.choices[0].code.as_str()))
                    })
                    .collect();
                Some(ic_product::option_code(&parts))
            } else {
                spec.excess_options.first().map(|e| e.code.clone())
            };
        }
        open.lives[0].factors.date_of_birth = NaiveDate::from_ymd_opt(today.year() - age, 1, 1);
        open.lives[0].factors.occupation_class = class.map(Into::into);

        // The quote, as the adviser and customer screens ask the core for it.
        let application = IssuePolicy {
            continuation_of: None,
            policy: open.policy.clone(),
            number: open.policy_number.clone(),
            group: open.group.clone(),
            policyholder: open.policyholder.clone(),
            product_code: product.into(),
            inception: today,
            frequency: open.frequency,
            lives: vec![LifeSpec {
                id: open.lives[0].life.clone(),
                kind: LifeKind::Named {
                    party: open.lives[0].party.clone(),
                },
                factors: open.lives[0].factors.clone(),
            }],
            covers: open
                .covers
                .iter()
                .map(|c| CoverSpec {
                    excess_option: c.option.clone(),
                    code: c.code.clone(),
                    life: Some(c.life.clone()),
                    sum_insured_minor: c.sum_insured_minor,
                    loadings: vec![],
                    exclusions: vec![],
                })
                .collect(),
            migrated: false,
        };
        let (hash, quote) = self.with_cluster(|c| {
            let catalog = &c.state(0).catalog;
            let hash = catalog.product_on(product, today).unwrap().content_hash.clone();
            (hash, ic_core::quote(catalog, application, today))
        });
        let quote = quote.unwrap_or_else(|r| panic!("{product} could not be quoted: {r}"));
        let quoted_minor = modal_total(&quote);
        assert!(quoted_minor > 0, "{product}: a quote of nothing");
        open.quote.product_hash = hash.clone();
        open.quote.premium_minor = quoted_minor;
        (open, hash, quoted_minor)
    }

    /// Close `n` days, the customer paying what is owed each morning; returns what was paid.
    fn months(&self, n: u32, policy: &PolicyId) -> i64 {
        let until = self.today().checked_add_months(chrono::Months::new(n)).unwrap();
        self.until(until, policy)
    }

    /// Close days to `until`, the policyholder paying what is owed each day.
    fn until(&self, until: NaiveDate, policy: &PolicyId) -> i64 {
        self.with_cluster(|c| {
            let mut paid = 0;
            while c.business_date().unwrap() < until {
                let owing = c.state(0).policies[policy].outstanding_minor();
                if owing > 0 {
                    let pay = CoreCommand::ApplyReceipt {
                        policy: policy.clone(),
                        receipt: format!("{policy}-r{}", c.business_date().unwrap()),
                        amount: Money::new(owing, NZD),
                    };
                    c.submit(0, "cash-matching", pay).unwrap();
                    paid += owing;
                }
                let close = c.end_of_day().unwrap();
                assert!(close.breaks.is_empty(), "breaks on {}: {:?}", close.closed, close.breaks);
            }
            assert_eq!(c.state(0).policies[policy].status, PolicyStatus::InForce, "still in force on {until}");
            paid
        })
    }

    fn claims(&self, command: claims::Command) {
        let name = command.name();
        self.with_cluster(|c| {
            if let Err(e) = c.submit(0, "claims", wrap(command)) {
                panic!("{name} was refused: {e}");
            }
            let breaks = c.reconcile();
            assert!(breaks.is_empty(), "{breaks:?}");
        });
    }

    fn claim(&self, id: &str) -> claims::Claim {
        self.with_cluster(|c| c.state(0).claims.claims[id].clone())
    }

    fn today(&self) -> NaiveDate {
        self.with_cluster(|c| c.business_date().unwrap())
    }

    fn settle(&self, policy: &PolicyId, payment: &str, minor: i64) {
        self.with_cluster(|c| {
            let settle = CoreCommand::SettlePayable {
                policy: policy.clone(),
                payment: payment.into(),
                payable: PayableKind::Benefit,
                refund: None,
                amount: Money::new(minor, NZD),
                bank_account: CLAIMS_BANK.into(),
            };
            c.submit(0, "payments", settle).unwrap();
            assert!(c.reconcile().is_empty());
            let l = &c.state(0).ledger;
            assert_eq!(l.natural_balance(&accounts::benefit_payable(policy, NZD).id), 0, "nothing left owed");
        });
    }

    /// Notify a claim on a cover, acknowledge it, give it to an assessor and check the cover.
    fn notify(&self, claim: &str, policy: &PolicyId, cover: &str, life: &str, claim_type: ClaimType, event_date: NaiveDate) {
        self.claims(notification(claim, policy, cover, life, claim_type, event_date));
        self.claims(claims::Command::Acknowledge {
            claim: claim.into(),
            delivery_ref: "delivered".into(),
        });
        self.claims(claims::Command::Assign {
            claim: claim.into(),
            to: "sam".into(),
            actor: actor("sam"),
        });
        self.claims(claims::Command::RecordCause {
            claim: claim.into(),
            cause_code: "CC-0001".into(),
            actor: actor("sam"),
        });
        self.claims(claims::Command::CoverCheck {
            claim: claim.into(),
            relied_on: vec!["in_force".into()],
            actor: actor("sam"),
        });
        let facts = self.claim(claim).cover_check.unwrap().facts;
        assert!(facts.in_force, "{claim}: the cover is in force on the event date");
    }

    fn admit(&self, claim: &str, amount: Option<i64>, periodic: Option<Periodic>, payer_deceased: bool) {
        self.claims(claims::Command::Decide {
            claim: claim.into(),
            request: Request::Admit {
                amount_minor: amount,
                periodic,
                exclusion_override: None,
                exclusion_reviewed: false,
                payer_deceased,
            },
            reasons: vec!["R-1".into()],
            actor: actor("sam"),
        });
        self.claims(claims::Command::VerifyPayee {
            claim: claim.into(),
            party: "party-claimant".into(),
            kind: PayeeKind::Policyholder,
            account_ref: "acct-1".into(),
            name_check_ref: "name-1".into(),
            screening_ref: "screen-1".into(),
            authority_doc: None,
            actor: actor("sam"),
        });
    }

    /// A lump sum claim admitted at the sum insured, paid and settled.
    fn lump_sum(&self, claim: &str, policy: &PolicyId, cover: &str, life: &str, claim_type: ClaimType, sum: i64) {
        let today = self.today();
        let died = claim_type == ClaimType::Death;
        // A living benefit is paid once the life has survived the cover's survival period, so
        // the event is a month back; a death is notified on the day.
        let event = if died { today } else { today - chrono::Days::new(30) };
        self.notify(claim, policy, cover, life, claim_type, event);
        assert_eq!(self.claim(claim).cover_check.unwrap().facts.sum_insured_minor, sum, "{claim}");
        self.admit(claim, None, None, died);
        assert_eq!(self.claim(claim).payable_minor, sum, "{claim}: admitted at the sum insured");
        self.claims(claims::Command::Pay {
            claim: claim.into(),
            payment_ref: format!("{claim}-pay"),
            amount_minor: sum,
            reissue_of: None,
            actor: actor("sam"),
        });
        self.settle(policy, &format!("{claim}-pay"), sum);
        assert_eq!(self.claim(claim).status, Status::Paid);
        if died {
            self.with_cluster(|c| assert_eq!(c.state(0).policies[policy].status, PolicyStatus::Terminated, "{claim}"));
        }
    }

    /// A health provider's invoice, admitted by the rules and paid to the provider by a run.
    fn invoice(&self, claim: &str, policy: &PolicyId, cover: &str, category: &str, minor: i64) {
        let today = self.today();
        let claims::Command::Notify(mut n) = notification(claim, policy, cover, "L1", ClaimType::Health, today) else {
            unreachable!()
        };
        n.channel = Channel::Provider;
        n.notifier = Notifier::Provider;
        n.health = Some(claims::HealthItem {
            category: category.into(),
            invoice_minor: minor,
            provider: Some("provider-7".into()),
            pre_approval_request: false,
            settles_pre_approval: None,
            service_code: None,
            service_date: None,
        });
        self.claims(claims::Command::Notify(n));
        let record = self.claim(claim);
        assert_eq!((record.status, record.admitted_minor), (Status::Admitted, minor), "{claim}");
        let run = format!("{claim}-run");
        self.claims(claims::Command::PrepareRun {
            run: run.clone(),
            kind: RunKind::Provider,
            benefit_date: today,
            actor: actor("sue"),
        });
        self.claims(claims::Command::ApproveRun {
            run: run.clone(),
            actor: actor("sam"),
        });
        self.settle(policy, &format!("{run}/{claim}/{today}"), minor);
        assert_eq!(self.claim(claim).status, Status::Paid);
    }

    /// An income claim admitted at the monthly benefit, its first month paid by a benefit run.
    fn income(&self, claim: &str, policy: &PolicyId, cover: &str, monthly: i64) {
        let disabled = self.today();
        self.notify(claim, policy, cover, "L1", ClaimType::IncomeProtection, disabled);
        // The benefit starts after the waiting period chosen and runs for the period the cover's
        // option sets, as the claim facts give them.
        let facts = self.claim(claim).cover_check.unwrap().facts;
        let months = facts.benefit_months.expect("the benefit period");
        let first = disabled.checked_add_days(chrono::Days::new(u64::from(facts.waiting_days))).unwrap();
        self.with_cluster(|c| {
            while c.business_date().unwrap() < first {
                let close = c.end_of_day().unwrap();
                assert!(close.breaks.is_empty(), "{:?}", close.breaks);
            }
        });
        let periodic = Periodic {
            monthly_minor: monthly,
            first_period: first,
            end: first.checked_add_months(chrono::Months::new(months)).unwrap(),
            expected_end: first.checked_add_months(chrono::Months::new(months.min(6))).unwrap(),
        };
        self.admit(claim, None, Some(periodic), false);
        let run = format!("{claim}-run");
        self.claims(claims::Command::PrepareRun {
            run: run.clone(),
            kind: RunKind::Benefit,
            benefit_date: first,
            actor: actor("sue"),
        });
        self.claims(claims::Command::ApproveRun {
            run: run.clone(),
            actor: actor("sam"),
        });
        self.settle(policy, &format!("{run}/{claim}/{first}"), monthly);
    }
}

fn notification(claim: &str, policy: &PolicyId, cover: &str, life: &str, claim_type: ClaimType, event_date: NaiveDate) -> claims::Command {
    claims::Command::Notify(Box::new(Notification {
        claim: claim.into(),
        policy: policy.0.clone(),
        cover: cover.into(),
        life: Some(life.into()),
        claim_type,
        event_date,
        channel: Channel::App,
        notifier: Notifier::Policyholder,
        claimant: Some("party-claimant".into()),
        claimed_minor: None,
        flags: Flags::default(),
        health: None,
        underwriters: vec![],
        catastrophe_event: None,
        cause_code: None,
    }))
}

/// The premium a period of the quote asks for, all in.
fn modal_total(q: &ic_product::rating::ModalPremium) -> i64 {
    let covers: i64 = q.by_cover.values().map(|m| m.minor).sum();
    let gst: i64 = q.gst.values().map(|m| m.minor).sum();
    let levies: i64 = q.levies.values().map(|m| m.minor).sum();
    covers + gst + levies + q.policy_fee.minor
}

/// Three months billed and paid: each month's bill is the premium quoted.
async fn billed_as_quoted(b: &Bench, issued: &Issued) {
    let paid = b.months(3, &issued.policy);
    assert_eq!(paid, 3 * issued.quoted_minor, "three monthly premiums, each as quoted");
}

// ---- The products ----------------------------------------------------------------------

/// Purpose: AIA Living, with life cover, Critical Conditions and income protection on one life,
/// is quoted by the core, underwritten straight through, reserved with reinsurance and issued
/// on the quoted version; AIA's own Interim Cover is written while it is underwritten and ended
/// once the policy is issued; three months are billed as quoted and paid; and a Critical
/// Conditions claim is admitted at the sum insured and paid.
/// Why it matters: this is AIA's main retail product. Each module holds part of it, so only a
/// journey through all of them shows that a customer can buy it, pay for it and be paid by it.
#[tokio::test]
async fn aia_living_from_quote_to_a_paid_trauma_claim() {
    let b = bench();
    let issued = b
        .apply(
            "LIV",
            "aia-living",
            &[
                ("life", "life", 500_000_00, None),
                ("critical-conditions", "trauma", 150_000_00, None),
                ("income-agreed-value", "income_protection", 4_000_00, Some("waiting=4w,period=to-65")),
            ],
            40,
            Some("2"),
        )
        .await;
    b.with_cluster(|c| {
        let interim = &c.state(0).policies[&PolicyId::from("POL-LIV-interim")];
        assert_eq!((interim.product_code.as_str(), interim.status), ("aia-interim", PolicyStatus::Cancelled));
    });
    billed_as_quoted(&b, &issued).await;
    b.lump_sum("clm-liv", &issued.policy, "critical-conditions", "L1", ClaimType::Trauma, 150_000_00);
}

/// Purpose: AIA Living for business, with life cover for a key person, goes from quote to a
/// policy through underwriting and reinsurance, is billed as quoted, and a death claim is paid
/// and ends the policy.
/// Why it matters: the business range has its own product and rates; without this journey a
/// business owner's policy could be defined and still not be sold or claimed on.
#[tokio::test]
async fn aia_living_business_from_quote_to_a_paid_death_claim() {
    let b = bench();
    let issued = b
        .apply("BUS", "aia-living-business", &[("life", "life", 1_000_000_00, None)], 45, Some("1"))
        .await;
    billed_as_quoted(&b, &issued).await;
    b.lump_sum("clm-bus", &issued.policy, "life", "L1", ClaimType::Death, 1_000_000_00);
}

/// Purpose: Start-Up Income Protection is quoted for an occupation class, underwritten,
/// issued, billed as quoted, and an income claim is admitted at the monthly benefit and its
/// first month paid by a benefit run.
/// Why it matters: an income cover pays monthly through runs, a different money path from a
/// lump sum; it has to work for the product's own option menu and rate table.
#[tokio::test]
async fn aia_start_up_income_from_quote_to_a_paid_benefit_month() {
    let b = bench();
    let issued = b
        .apply(
            "SUI",
            "aia-start-up-income",
            &[("start-up-income", "income_protection", 3_000_00, None)],
            30,
            Some("2"),
        )
        .await;
    billed_as_quoted(&b, &issued).await;
    b.income("clm-sui", &issued.policy, "start-up-income", 3_000_00);
}

/// Purpose: a business owner's Start-Up Income moves to AIA Living Business Business
/// Continuity after three years of trading: the request is quoted by the core at the owner's
/// age, checked by a person on the financial queue, checked again by the driver against the
/// Start-Up policy in the core, issued as a new policy with no health questions, and the
/// Start-Up policy is then cancelled in the core. A second pass delivers nothing more.
/// Why it matters: this is the facility working through the real core and products; with
/// the Start-Up policy left running the business would pay for, and could claim, two income
/// benefits for the same loss.
#[tokio::test]
async fn aia_start_up_income_moves_to_business_continuity_after_three_years() {
    let b = bench();
    let start_up = b
        .apply(
            "SUI",
            "aia-start-up-income",
            &[("start-up-income", "income_protection", 3_000_00, None)],
            30,
            Some("2"),
        )
        .await;
    let (mut open, hash, _) = b.quoted(
        "BCM",
        "aia-living-business",
        &[("business-continuity-agreed-value", "income_protection", 3_000_00, None)],
        30,
        Some("2"),
    );
    open.entry = ic_underwriting::model::Entry::Facility {
        facility: ic_underwriting::model::Facility::BusinessContinuity,
        from_policy: start_up.policy.clone(),
        from_group: GroupId::from("grp-SUI"),
        from_life: "L1".into(),
        from_cover: "start-up-income".into(),
        trigger_date: start().checked_sub_months(chrono::Months::new(40)).unwrap(),
        evidence: "vault://accounts/sui".into(),
    };
    b.module.with(|w| w.ok("new-business", Command::OpenProposal(Box::new(open))));
    let case = b.module.with(|w| w.take("BCM", "L1", "amy"));
    b.module.with(|w| {
        w.ok(
            "underwriting-console",
            Command::CheckOptionEvidence {
                case: case.clone(),
                underwriter: "amy".into(),
                accepted: true,
                note: String::new(),
            },
        )
    });
    for _ in 0..4 {
        let pass: Pass = b.driver.deliver().await.unwrap();
        assert!(pass.parked.is_empty(), "{pass:?}");
    }
    let status = b.module.with(|w| w.p("BCM").status.clone());
    assert!(matches!(status, ProposalStatus::Issued { .. }), "{status:?}");
    b.with_cluster(|c| {
        let policies = &c.state(0).policies;
        let moved = &policies[&PolicyId::from("POL-BCM")];
        assert_eq!((moved.status, moved.product_hash.as_str()), (PolicyStatus::InForce, hash.as_str()));
        assert_eq!(moved.covers["business-continuity-agreed-value"].sum_insured_on(start()), 3_000_00);
        assert_eq!(policies[&start_up.policy].status, PolicyStatus::Cancelled);
    });
    let pass: Pass = b.driver.deliver().await.unwrap();
    assert_eq!((pass.delivered, pass.parked.len()), (0, 0));
}

/// Purpose: Cover for Life, whole of life cover with no minimum amount, is quoted, underwritten,
/// issued, billed as quoted, and a death claim is paid and ends the policy.
/// Why it matters: Cover for Life has its own premium basis (paid to an age); this shows its
/// premiums are the ones quoted and its benefit is paid.
#[tokio::test]
async fn aia_cover_for_life_from_quote_to_a_paid_death_claim() {
    let b = bench();
    let issued = b.apply("CFL", "aia-cover-for-life", &[("life", "life", 25_000_00, None)], 60, None).await;
    billed_as_quoted(&b, &issued).await;
    b.lump_sum("clm-cfl", &issued.policy, "life", "L1", ClaimType::Death, 25_000_00);
}

/// Purpose: Essentials Life is quoted, underwritten, issued, billed as quoted, and a death
/// claim is paid and ends the policy.
/// Why it matters: Essentials is sold direct with simple questions; the journey proves that
/// path reaches a policy and a payment like any other.
#[tokio::test]
async fn aia_essentials_life_from_quote_to_a_paid_death_claim() {
    let b = bench();
    let issued = b.apply("ESS", "aia-essentials-life", &[("life", "life", 200_000_00, None)], 50, None).await;
    billed_as_quoted(&b, &issued).await;
    b.lump_sum("clm-ess", &issued.policy, "life", "L1", ClaimType::Death, 200_000_00);
}

/// Purpose: an AIA Starter plan with life cover is quoted, underwritten, issued, billed as
/// quoted, and a death claim is paid and ends the policy.
/// Why it matters: Starter plans are fixed packages for young customers; the journey shows the
/// package can be bought and claimed on.
#[tokio::test]
async fn aia_starter_from_quote_to_a_paid_death_claim() {
    let b = bench();
    let issued = b.apply("STA", "aia-starter", &[("life", "life", 100_000_00, None)], 25, None).await;
    billed_as_quoted(&b, &issued).await;
    b.lump_sum("clm-sta", &issued.policy, "life", "L1", ClaimType::Death, 100_000_00);
}

/// Purpose: while an AIA Living application is being underwritten, the life is covered by AIA's
/// Interim Cover, written by the underwriting module at the moment the proposal opens; a death
/// in that time is claimed on the interim policy and paid at the cover written.
/// Why it matters: Interim Cover is never sold on its own: it exists only between application
/// and decision. The only way to show it pays is to claim on it in that window.
#[tokio::test]
async fn aia_interim_cover_pays_a_death_while_the_application_is_underwritten() {
    let b = bench();
    let mut open = b.module.with(|w| w.open_command("INT", &["L1"], &[("L1", "life", "life", 400_000_00)], false));
    open.product_code = "aia-living".into();
    open.group = GroupId::from("grp-INT");
    open.lives[0].factors.date_of_birth = Some(d(1987, 1, 1));
    b.module.with(|w| w.ok("new-business", Command::OpenProposal(Box::new(open))));
    let pass = b.driver.deliver().await.unwrap();
    assert!(pass.parked.is_empty(), "{pass:?}");
    let interim = PolicyId::from("POL-INT-interim");
    let sum = b.with_cluster(|c| {
        let p = &c.state(0).policies[&interim];
        assert_eq!((p.product_code.as_str(), p.status), ("aia-interim", PolicyStatus::InForce));
        p.covers["life"].layers[0].sum_insured_minor
    });
    assert_eq!(sum, 400_000_00, "no more than the cover applied for");
    b.with_cluster(|c| {
        let close = c.end_of_day().unwrap();
        assert!(close.breaks.is_empty());
    });
    b.lump_sum("clm-int", &interim, "life", "L1", ClaimType::Death, 400_000_00);
}

/// Purpose: AIA Private Health is quoted, underwritten, issued, billed as quoted, and a
/// surgical invoice from a provider is admitted by the rules and paid to the provider in a run.
/// Why it matters: health cover is paid to providers, invoice by invoice, by a run, not to the
/// customer; the product has to reach that path with its own limits.
#[tokio::test]
async fn aia_private_health_from_quote_to_a_paid_provider_invoice() {
    let b = bench();
    let issued = b
        .apply("APH", "aia-private-health", &[("private-health", "health", 300_000_00, None)], 35, None)
        .await;
    billed_as_quoted(&b, &issued).await;
    b.invoice("clm-aph", &issued.policy, "private-health", "surgery", 8_500_00);
}

/// Purpose: AIA Cancer Care is quoted, underwritten, issued, billed as quoted, and a cancer
/// treatment invoice is admitted by the rules and paid to the provider in a run.
/// Why it matters: Cancer Care is a health product with one purpose; the journey shows a
/// treatment bill on it is paid.
#[tokio::test]
async fn aia_cancer_care_from_quote_to_a_paid_treatment_invoice() {
    let b = bench();
    let issued = b
        .apply("ACC", "aia-cancer-care", &[("cancer-care", "health", 300_000_00, None)], 45, None)
        .await;
    billed_as_quoted(&b, &issued).await;
    b.invoice("clm-acc", &issued.policy, "cancer-care", "cancer-treatment", 12_000_00);
}

/// Purpose: an AIA Corporate Solutions scheme with group life for its staff is issued on the
/// core with its members, quoted per member, billed to the employer as quoted, and a member's
/// death is claimed on the scheme and paid, without ending the scheme.
/// Why it matters: group schemes are not underwritten one by one; the scheme is issued with its
/// members and billed to the employer. A member's claim must be paid and the scheme go on.
#[tokio::test]
async fn aia_corporate_from_scheme_to_a_paid_member_death_claim() {
    let b = bench();
    let members: Vec<LifeSpec> = (1..=3)
        .map(|n| LifeSpec {
            id: LifeId(format!("M{n}")),
            kind: LifeKind::UnnamedMember {
                member_ref: format!("M{n}"),
                linked_party: None,
            },
            factors: LifeFactors {
                date_of_birth: Some(d(1980 + n, 6, 1)),
                category: Some("staff".into()),
                salary_minor: Some(80_000_00),
                ..Default::default()
            },
        })
        .collect();
    let scheme = IssuePolicy {
        continuation_of: None,
        policy: PolicyId::from("POL-CORP"),
        number: "AIAC0001".into(),
        group: GroupId::from("grp-CORP"),
        policyholder: PartyId::from("employer-1"),
        product_code: "aia-corporate".into(),
        inception: start(),
        frequency: Frequency::Monthly,
        lives: members,
        covers: vec![CoverSpec {
            excess_option: None,
            code: "group-life".into(),
            life: None,
            sum_insured_minor: 0,
            loadings: vec![],
            exclusions: vec![],
        }],
        migrated: false,
    };
    let quoted_minor = b.with_cluster(|c| {
        let q = ic_core::quote(&c.state(0).catalog, scheme.clone(), start()).unwrap();
        c.submit(0, "new-business", CoreCommand::IssuePolicy(Box::new(scheme))).unwrap();
        modal_total(&q)
    });
    assert!(quoted_minor > 0);
    let issued = Issued {
        policy: PolicyId::from("POL-CORP"),
        quoted_minor,
    };
    billed_as_quoted(&b, &issued).await;
    let today = b.today();
    b.notify("clm-corp", &issued.policy, "group-life", "M2", ClaimType::Death, today);
    let facts = b.claim("clm-corp").cover_check.unwrap().facts;
    let benefit = facts.sum_insured_minor;
    assert!(benefit > 0, "the member's benefit: {facts:?}");
    b.admit("clm-corp", None, None, false);
    b.claims(claims::Command::Pay {
        claim: "clm-corp".into(),
        payment_ref: "clm-corp-pay".into(),
        amount_minor: benefit,
        reissue_of: None,
        actor: actor("sam"),
    });
    b.settle(&issued.policy, "clm-corp-pay", benefit);
    b.with_cluster(|c| assert_eq!(c.state(0).policies[&issued.policy].status, PolicyStatus::InForce, "the scheme goes on"));
}

/// Purpose: an AIA Living life with accelerated Critical Conditions and both buy-backs chosen
/// has a full trauma claim paid, which takes the trauma cover and as much life cover off; a year
/// on, the life cover and then the trauma cover are written again without evidence, at most
/// once each, and only inside the window after the survival period; the new cover is billed.
/// Why it matters: the buy-backs are optional benefits the customer pays extra for (1105 AL-CC
/// sections 18 and 19). Without them the customer pays for a promise the system cannot keep;
/// given early, twice or outside the window they are cover nobody priced.
#[tokio::test]
async fn aia_living_cover_is_bought_back_a_year_after_an_accelerated_trauma_claim() {
    use ic_core::buyback::BuyBack;
    let b = bench();
    let issued = b
        .apply(
            "BBK",
            "aia-living",
            &[
                ("life", "life", 500_000_00, None),
                ("critical-conditions-accelerated", "trauma", 200_000_00, Some("buyback=yes,life-buyback=yes")),
            ],
            40,
            Some("2"),
        )
        .await;
    let policy = issued.policy.clone();
    // The claim's event is a month back, so the cover has run two months.
    b.months(2, &policy);
    b.lump_sum("clm-bbk", &policy, "critical-conditions-accelerated", "L1", ClaimType::Trauma, 200_000_00);
    let held = |b: &Bench, cover: &str| {
        b.with_cluster(|c| {
            let on = c.business_date().unwrap();
            c.state(0).policies[&policy].covers[cover].layers.iter().filter(|l| l.in_force_on(on)).map(|l| l.sum_insured_minor).sum::<i64>()
        })
    };
    assert_eq!((held(&b, "life"), held(&b, "critical-conditions-accelerated")), (300_000_00, 0));
    let paid_on = b.today();
    let ask = |restores: &str, after_days: u32, reference: &str| {
        CoreCommand::BuyBack(Box::new(BuyBack {
            policy: policy.clone(),
            claim: "clm-bbk".into(),
            restores: restores.into(),
            life: LifeId::from("L1"),
            after_days,
            reference: reference.into(),
        }))
    };
    let send = |b: &Bench, command: CoreCommand| b.with_cluster(|c| c.submit(0, "policy-servicing", command));
    let refused = |r: Result<(), ic_core::Reject>| r.unwrap_err().code;

    assert_eq!(refused(send(&b, ask("life", 365, "bb-1"))), "buyback_window");
    assert_eq!(refused(send(&b, ask("income", 365, "bb-1"))), "no_buyback");
    assert_eq!(refused(send(&b, ask("life", 90, "bb-1"))), "buyback_window");
    assert_eq!(refused(send(&b, ask("life", 365, "bb 1"))), "invalid_request");
    let other = CoreCommand::BuyBack(Box::new(BuyBack {
        policy: policy.clone(),
        claim: "clm-none".into(),
        restores: "life".into(),
        life: LifeId::from("L1"),
        after_days: 365,
        reference: "bb-9".into(),
    }));
    assert_eq!(refused(send(&b, other)), "unknown_claim");
    // Twelve months and a day after the claim took the cover.
    while b.today() <= paid_on + chrono::Days::new(365) {
        b.months(1, &policy);
    }
    assert!(b.today() <= paid_on + chrono::Days::new(365 + 60));
    // What the claim took is written again beside the cover left (indexed at the anniversary).
    let left = held(&b, "life");
    send(&b, ask("life", 365, "bb-1")).unwrap();
    send(&b, ask("life", 365, "bb-1")).unwrap();
    assert_eq!(refused(send(&b, ask("life", 365, "bb-2"))), "buyback_used");
    assert_eq!(refused(send(&b, ask("critical-conditions-accelerated", 365, "bb-1"))), "duplicate_request");
    assert_eq!(held(&b, "life"), left + 200_000_00);
    send(&b, ask("critical-conditions-accelerated", 365, "bb-3")).unwrap();
    assert_eq!(held(&b, "critical-conditions-accelerated"), 200_000_00);
    let before = b.with_cluster(|c| c.state(0).policies[&policy].outstanding_minor());
    assert!(before > 0, "the days left of the period billed are billed for the cover written again");
    b.months(1, &policy);
}

/// Purpose: while a trauma claim is open on an AIA Living Critical Conditions cover, the cover
/// is neither increased without evidence on a special event nor converted to accelerated
/// trauma; both are refused as claim_open with nothing written.
/// Why it matters: AIA's wordings give no increase or conversion without evidence while a claim
/// is made. Allowed, a customer who has just fallen ill could add or move cover on the strength
/// of the very condition being claimed, with no one ever assessing it.
#[tokio::test]
async fn aia_living_cover_is_not_increased_or_converted_while_a_claim_is_open() {
    let b = bench();
    let issued = b
        .apply("OPN", "aia-living", &[("life", "life", 500_000_00, None), ("critical-conditions", "trauma", 200_000_00, None)], 40, Some("2"))
        .await;
    let policy = issued.policy.clone();
    b.months(1, &policy);
    let today = b.today();
    b.notify("clm-opn", &policy, "critical-conditions", "L1", ClaimType::Trauma, today);
    b.with_cluster(|c| assert!(c.state(0).policies[&policy].claims["clm-opn"].open));
    let send = |command: CoreCommand| b.with_cluster(|c| c.submit(0, "policy-servicing", command));
    let increase = CoreCommand::ExerciseOption(Box::new(ic_core::command::OptionExercise {
        policy: policy.clone(),
        cover: "critical-conditions".into(),
        life: LifeId::from("L1"),
        event: "marriage".into(),
        event_date: today,
        sum_insured_minor: 10_000_00,
        evidence: "vault/evidence-1".into(),
        reference: "se-1".into(),
        from_cover: None,
        excess_option: None,
    }));
    assert_eq!(send(increase).unwrap_err().code, "claim_open");
    let option = b.with_cluster(|c| {
        c.state(0).policies[&policy].covers["critical-conditions"].layers[0].excess_option.clone().unwrap_or_default()
    });
    let convert = CoreCommand::ConvertCover(Box::new(ic_core::conversion::ConvertCover {
        policy: policy.clone(),
        cover: "critical-conditions".into(),
        life: LifeId::from("L1"),
        sum_insured_minor: 200_000_00,
        to_cover: "critical-conditions-accelerated".into(),
        excess_option: option,
        reference: "sc-1".into(),
        evidence: None,
    }));
    assert_eq!(send(convert).unwrap_err().code, "claim_open");
    b.with_cluster(|c| {
        let p = &c.state(0).policies[&policy];
        assert!(p.option_uses.is_empty() && p.conversions.is_empty());
        assert!(!p.covers.contains_key("critical-conditions-accelerated"));
    });
}
