//! The check made before a servicing request that moves value or cover.
//!
//! A surrender, a withdrawal, a payout, a cancellation or a reduction of
//! cover is first put to the customer master (`docs/customers.md`), which
//! knows what this API does not: whether the policyholder asked for a pause
//! before such an act, whether there is a risk of financial abuse, and
//! whether the person asking holds an authority to act for them.
//!
//! Nothing here is a refusal of the policyholder's own decision. A pause
//! answers 409 and says what is needed; the same request sent again with
//! the reference of the conversation (and of the specialist's review, where
//! one was asked for) goes through. Only a representative with no authority
//! for the act is refused. Where the customer master cannot answer, the
//! request is not sent on: the caller tries again.

use std::future::Future;
use std::pin::Pin;

use chrono::NaiveDate;
use ic_core::invest::{InvestCommand, OrderKind};
use ic_core::{Command, PartyId, PolicyId};
use serde_json::Value;

/// The header carrying the reference of the conversation held with the
/// policyholder by a trained person.
pub const CONVERSATION: &str = "x-ic-conversation";
/// The header carrying the reference of the specialist's review.
pub const REVIEW: &str = "x-ic-specialist-review";
/// The header naming the party asking, where it is not the policyholder.
pub const ACTING_PARTY: &str = "x-ic-acting-party";

type Asked<'a> = Pin<Box<dyn Future<Output = Result<(u16, Value), String>> + Send + 'a>>;

/// The customer master, as servicing asks it.
pub trait Customers: Send + Sync {
    /// `GET /v1/parties/{id}/routing?act=`.
    fn routing(&self, party: &PartyId, act: &str) -> Asked<'_>;
    /// `GET /v1/parties/{id}/authority?actor=&act=&on=`.
    fn authority(&self, party: &PartyId, actor: &PartyId, act: &str, on: NaiveDate) -> Asked<'_>;
}

/// The policy a servicing command acts on and the act it is, in the
/// customer master's words, where the command moves value out of the
/// policy or takes cover away. Other commands are not checked.
pub fn act(command: &Command) -> Option<(&PolicyId, &'static str)> {
    match command {
        Command::Investment(c) => match &**c {
            InvestCommand::Surrender { policy, .. } | InvestCommand::RaisePayout { policy, .. } | InvestCommand::DirectPayout { policy, .. } => {
                Some((policy, "surrender"))
            }
            InvestCommand::PlaceOrder(order) if matches!(order.kind, OrderKind::Withdrawal | OrderKind::RegularWithdrawal) => {
                Some((&order.policy, "surrender"))
            }
            _ => None,
        },
        Command::CancelPolicy { policy } => Some((policy, "cover")),
        Command::ReduceCover(c) => Some((&c.policy, "cover")),
        Command::MakePaidUp(c) => Some((&c.policy, "cover")),
        Command::SuspendCover(c) => Some((&c.policy, "cover")),
        _ => None,
    }
}

/// What the check decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Send the command on.
    Proceed,
    /// Hold it: `code` says what is needed before it is sent again.
    Hold { code: &'static str, detail: &'static str },
    /// The person asking may not do this for the policyholder.
    Refuse,
    /// The customer master did not answer; try again.
    Unavailable(String),
}

/// What the caller says has already happened, from the request's headers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Given {
    pub conversation: Option<String>,
    pub review: Option<String>,
    /// The party asking, where it is not the policyholder.
    pub acting: Option<PartyId>,
}

fn reference(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= 128 && r.chars().all(|c| c.is_ascii_graphic()))
        .map(str::to_string)
}

impl Given {
    pub fn from_headers(get: impl Fn(&str) -> Option<String>) -> Self {
        Self {
            conversation: reference(get(CONVERSATION).as_deref()),
            review: reference(get(REVIEW).as_deref()),
            acting: reference(get(ACTING_PARTY).as_deref()).map(PartyId),
        }
    }
}

/// Put one act on a policyholder's policy to the customer master.
pub async fn check(customers: &dyn Customers, policyholder: &PartyId, act: &str, on: NaiveDate, given: &Given) -> Outcome {
    // Someone acting for the policyholder is asked about first: no
    // conversation makes an act theirs to take.
    if let Some(actor) = given.acting.as_ref().filter(|a| *a != policyholder) {
        match customers.authority(policyholder, actor, act, on).await {
            Ok((200, body)) => match body["decision"].as_str() {
                Some("allow") => {}
                Some("route") => {
                    return Outcome::Hold {
                        code: "authority_in_question",
                        detail: "whether this person may act for the policyholder is being decided; nothing changes on the policy meanwhile",
                    }
                }
                _ => return Outcome::Refuse,
            },
            Ok((status, _)) => return Outcome::Unavailable(format!("the customer master answered {status} about the authority")),
            Err(e) => return Outcome::Unavailable(e),
        }
    }
    match customers.routing(policyholder, act).await {
        Ok((200, body)) => {
            let asked = |field: &str| body[field].as_bool().unwrap_or(false);
            if asked("pause_for_conversation") && given.conversation.is_none() {
                Outcome::Hold {
                    code: "pause_for_conversation",
                    detail:
                        "a trained person talks with the policyholder first; send the request again with the conversation's reference and their decision stands",
                }
            } else if asked("specialist_review") && given.review.is_none() {
                Outcome::Hold {
                    code: "specialist_review",
                    detail: "a specialist reviews this request first; send it again with the review's reference",
                }
            } else {
                Outcome::Proceed
            }
        }
        // A party the master does not hold has no marker to respect.
        Ok((404, _)) => Outcome::Proceed,
        Ok((status, _)) => Outcome::Unavailable(format!("the customer master answered {status} about routing")),
        Err(e) => Outcome::Unavailable(e),
    }
}

/// The customer master service, reached with this API's own Entra token in
/// the app role `policy-servicing`.
pub struct Hosted {
    pub base: String,
    /// The master's application ID URI scope, such as `api://ic-customers/.default`.
    pub scope: String,
    pub credential: ic_azure::credential::AzureCredential,
    pub http: reqwest::Client,
}

fn encode(party: &PartyId) -> String {
    party.0.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

impl Hosted {
    async fn get(&self, path: String) -> Result<(u16, Value), String> {
        let token = self.credential.token(&self.scope).await.map_err(|e| e.to_string())?;
        let response = self
            .http
            .get(format!("{}{path}", self.base.trim_end_matches('/')))
            .bearer_auth(token)
            .header("x-ic-caller", "policy-servicing")
            .header("x-ic-purpose", "servicing")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }
}

impl Customers for Hosted {
    fn routing(&self, party: &PartyId, act: &str) -> Asked<'_> {
        let path = format!("/v1/parties/{}/routing?act={act}", encode(party));
        Box::pin(self.get(path))
    }
    fn authority(&self, party: &PartyId, actor: &PartyId, act: &str, on: NaiveDate) -> Asked<'_> {
        let path = format!("/v1/parties/{}/authority?actor={}&act={act}&on={on}", encode(party), encode(actor));
        Box::pin(self.get(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_core::invest::{Direction, NewOrder, OrderAmount, PayoutReason};
    use ic_core::LifeId;
    use ic_money::FundId;

    fn order(kind: OrderKind) -> Command {
        Command::Investment(Box::new(InvestCommand::PlaceOrder(Box::new(NewOrder {
            order_id: "o-1".into(),
            policy: PolicyId("pol-1".into()),
            fund: FundId("growth".into()),
            direction: Direction::Sell,
            amount: OrderAmount::AllUnits,
            kind,
            pricing_date: NaiveDate::from_ymd_opt(2027, 3, 1).unwrap(),
            switch_to: None,
        }))))
    }

    /// Purpose: the commands that take money out of a policy or take cover away are named
    /// with the act the customer master knows them by, and a command that does neither
    /// (a switch between funds, declining indexation) is not put to the master at all.
    /// Why it matters: this list is the whole of what a pause or a specialist's review can
    /// catch. A way of taking money out that is missing from it goes straight to the ledger
    /// for a person the insurer was told is at risk of financial abuse; a harmless change
    /// caught by it would hold up ordinary servicing for the people least able to chase it.
    #[test]
    fn value_and_cover_leaving_a_policy_are_the_acts_checked() {
        let policy = PolicyId("pol-1".into());
        let invest = |c: InvestCommand| Command::Investment(Box::new(c));
        let checked = [
            (
                invest(InvestCommand::Surrender {
                    policy: policy.clone(),
                    surrender: "s-1".into(),
                    pricing_dates: Default::default(),
                }),
                "surrender",
            ),
            (
                invest(InvestCommand::RaisePayout {
                    policy: policy.clone(),
                    payout: "p-1".into(),
                    amount_minor: 1,
                    reason: PayoutReason::Withdrawal,
                }),
                "surrender",
            ),
            (
                invest(InvestCommand::DirectPayout {
                    policy: policy.clone(),
                    payout: "p-1".into(),
                    payee: PartyId("party-1".into()),
                    payment: "pay-1".into(),
                    direction: "vault/dir-1".into(),
                }),
                "surrender",
            ),
            (order(OrderKind::Withdrawal), "surrender"),
            (order(OrderKind::RegularWithdrawal), "surrender"),
            (Command::CancelPolicy { policy: policy.clone() }, "cover"),
            (
                Command::ReduceCover(Box::new(ic_core::command::ReduceCover {
                    policy: policy.clone(),
                    cover: "life".into(),
                    life: LifeId("life-1".into()),
                    sum_insured_minor: 0,
                })),
                "cover",
            ),
            (
                Command::MakePaidUp(Box::new(ic_core::paid_up::MakePaidUp {
                    policy: policy.clone(),
                    cover: "life".into(),
                    life: LifeId("life-1".into()),
                    reference: "pu-1".into(),
                })),
                "cover",
            ),
            (
                Command::SuspendCover(Box::new(ic_core::suspension::SuspendCover {
                    policy: policy.clone(),
                    reference: "sus-1".into(),
                    from: NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(),
                    to: NaiveDate::from_ymd_opt(2028, 4, 1).unwrap(),
                })),
                "cover",
            ),
        ];
        for (command, expected) in &checked {
            assert_eq!(act(command), Some((&policy, *expected)), "{}", command.name());
        }
        let unchecked = [
            order(OrderKind::SwitchOut),
            invest(InvestCommand::CancelOrder { order_id: "o-1".into() }),
            Command::DeclineIndexation {
                policy: policy.clone(),
                declined: true,
            },
            Command::ResumeCover {
                policy: policy.clone(),
                reference: "sus-1".into(),
            },
        ];
        for command in &unchecked {
            assert_eq!(act(command), None, "{}", command.name());
        }
    }

    /// Purpose: a reference from a header is kept only when it is a short printable token,
    /// and the acting party is read the same way.
    /// Why it matters: the references are written to the log as the evidence that the
    /// conversation happened. A blank or a multi-line value accepted as a reference would let
    /// a request through with nothing behind it, or forge lines in the log.
    #[test]
    fn a_reference_is_a_short_printable_token() {
        let given = Given::from_headers(|name| match name {
            CONVERSATION => Some("  case-881  ".to_string()),
            REVIEW => Some("two words".to_string()),
            ACTING_PARTY => Some("party-9".to_string()),
            _ => None,
        });
        assert_eq!(given.conversation.as_deref(), Some("case-881"));
        assert_eq!(given.review, None);
        assert_eq!(given.acting, Some(PartyId("party-9".into())));
        assert_eq!(Given::from_headers(|_| Some(String::new())), Given::default());
        assert_eq!(Given::from_headers(|_| Some("x".repeat(129))), Given::default());
    }
}
