//! The wellness module for the portal's tests: its own state machine, with
//! one AIA Vitality member on a policy, behind the same JSON a node serves.

use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use chrono::NaiveDate;
use ic_channels::forward::{Source, REFUSED};
use ic_core::{LifeId, PolicyId};
use ic_log::StateMachine;
use ic_wellness::{Command, CoreFact, Envelope, WellnessState, CORE_EVENTS, PROGRAMME};
use serde_json::Value;

type Asked<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

pub struct WellnessModule(pub Mutex<WellnessState>);

/// A module where member `V-1` joined on `policy` and holds the initial discount.
pub fn seeded(policy: &PolicyId) -> WellnessModule {
    let mut s = WellnessState::new();
    let today = NaiveDate::from_ymd_opt(2027, 11, 1).unwrap();
    let send = |s: &mut WellnessState, index: u64, caller: &str, command: Command| {
        let applied = s.apply(
            index,
            Envelope {
                key: format!("seed-{index}"),
                caller: caller.to_string(),
                today,
                command,
            },
        );
        assert_eq!(applied.result, Ok(()), "{applied:?}");
    };
    let issued = CoreFact::Issued {
        policy: policy.clone(),
        product: "aia-living".into(),
        inception: NaiveDate::from_ymd_opt(2027, 1, 10).unwrap(),
    };
    send(&mut s, 1, CORE_EVENTS, Command::Core { fact: issued });
    let join = Command::Join {
        member: "V-1".into(),
        policy: policy.clone(),
        life: LifeId::from("life-1"),
        joined: today,
    };
    send(&mut s, 2, PROGRAMME, join);
    WellnessModule(Mutex::new(s))
}

impl Source for WellnessModule {
    fn ask(&self, question: Value) -> Asked<'_> {
        let answer = ic_wellness::answer(&self.0.lock().unwrap(), question).map_err(|e| format!("{REFUSED}{e}"));
        Box::pin(async move { answer })
    }

    fn tell(&self, _envelope: Value) -> Asked<'_> {
        Box::pin(async move { Err(format!("{REFUSED}the console sends wellness nothing")) })
    }
}
