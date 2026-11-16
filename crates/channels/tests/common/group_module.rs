//! The group module for the group console's tests: its own state machine
//! and its own test harness (scheme `harbour` from 1 November 2027 with
//! one founding member, E001), behind the routes the group service serves,
//! with the same codes and statuses. Seeded with one file, `f1`, whose
//! joiner E002 the customer master could not tell apart from party-x, so
//! a possible match waits for staff.

use std::sync::Mutex;

use ic_channels::group::{Answered, Group};
use ic_group::command::{caller, Envelope, GroupCommand, Resolution};
use ic_group::host::Question;
use ic_group::mapping::{FileKind, FileMapping};
use serde_json::{json, Value};

#[path = "../../../group/tests/common/mod.rs"]
pub mod harness;

use harness::{attrs, party, row, terms, Harness};

type Asked<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Answered, String>> + Send + 'a>>;

/// The salt the service's driver fingerprints a trial's rows with.
const SALT: &str = "test-salt";

pub struct GroupModule(pub Mutex<Harness>);

fn refused(status: u16, code: &str, message: &str) -> Answered {
    (status, json!({ "error": { "code": code, "message": message } }))
}

/// The module as seeded: a possible match on E002 waiting for staff.
pub fn seeded() -> GroupModule {
    let mut h = Harness::new(terms());
    h.upload(
        "f1",
        FileKind::Full,
        vec![
            row(1, "E001", attrs("staff", 60_000_00, "akl")),
            row(2, "E002", attrs("staff", 50_000_00, "akl")),
        ],
    )
    .unwrap();
    h.approve("f1").unwrap();
    h.resolve("f1", |id| {
        (id == "E002").then(|| Resolution::Possible {
            candidates: vec![party("party-x")],
        })
    });
    GroupModule(Mutex::new(h))
}

impl GroupModule {
    /// As the service's command route: the caller's key scoped by its name,
    /// a file received only by upload, a refusal 422 with its code.
    fn apply(&self, who: &str, key: &str, actor: &str, command: Value) -> Answered {
        let command: GroupCommand = match serde_json::from_value(command) {
            Ok(c) => c,
            Err(e) => return refused(400, "invalid_command", &e.to_string()),
        };
        if matches!(command, GroupCommand::ReceiveFile { .. }) {
            return refused(400, "use_upload", "a file is received by uploading it");
        }
        let mut h = self.0.lock().unwrap();
        let envelope = Envelope {
            key: format!("api:{who}:{key}"),
            caller: who.to_string(),
            actor: actor.to_string(),
            today: h.today(),
            command,
        };
        let outcome = h.g.apply_envelope(envelope);
        h.settle();
        match outcome.result {
            Ok(()) => (200, json!({ "applied": true, "duplicate": outcome.duplicate })),
            Err(reject) => refused(422, &reject.code, &reject.message),
        }
    }
}

impl Group for GroupModule {
    fn upload(&self, _: &str, _: &str, _: &str, _: Vec<u8>) -> Asked<'_> {
        Box::pin(async { Ok(refused(400, "not_here", "the group console uploads nothing")) })
    }

    fn command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let got = self.apply(caller::EMPLOYER, key, actor, command);
        Box::pin(async move { Ok(got) })
    }

    fn ask(&self, question: Value) -> Asked<'_> {
        let got = if question["ask"] == "outbox" {
            refused(403, "not_available", "the outbox is not served here")
        } else {
            match ic_group::host::answer(&self.0.lock().unwrap().g, question) {
                Ok(answer) => (200, answer),
                Err(e) if e.starts_with("not a question") => refused(400, "invalid_question", &e),
                Err(e) => refused(404, "not_found", &e),
            }
        };
        Box::pin(async move { Ok(got) })
    }

    fn staff_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let got = self.apply(caller::STAFF, key, actor, command);
        Box::pin(async move { Ok(got) })
    }

    fn member_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        let got = self.apply(caller::MEMBER, key, actor, command);
        Box::pin(async move { Ok(got) })
    }

    /// As the service's driver tries a mapping: sealed, read with the
    /// module's own CSV reader and mapping, and previewed.
    fn try_mapping(&self, scheme: &str, trial: Value) -> Asked<'_> {
        let got = (|| {
            let mapping: FileMapping = serde_json::from_value(trial["mapping"].clone()).map_err(|e| refused(400, "invalid_trial", &e.to_string()))?;
            let mapping = mapping.seal().map_err(|e| refused(422, "invalid_mapping", &e.to_string()))?;
            let sample = trial["sample"].as_str().unwrap_or_default();
            let table = ic_group::csv::parse(sample.as_bytes(), mapping.delimiter).map_err(|e| refused(422, "invalid_file", &e.to_string()))?;
            let mapped = mapping
                .map(&table, &format!("{SALT}:{scheme}"))
                .map_err(|e| refused(422, "invalid_mapping", &e.to_string()))?;
            let question = Question::Preview {
                scheme: scheme.to_string(),
                kind: mapping.kind,
                rows: mapped.rows,
                rejected: mapped.rejected,
            };
            let mut answer =
                ic_group::host::answer(&self.0.lock().unwrap().g, serde_json::to_value(question).unwrap()).map_err(|e| refused(404, "not_found", &e))?;
            answer["mapping"] = json!({ "id": mapping.id, "version": mapping.version, "kind": mapping.kind });
            Ok((200, answer))
        })()
        .unwrap_or_else(|e: Answered| e);
        Box::pin(async move { Ok(got) })
    }
}

/// For a browser: the employer's double and this module side by side. The
/// employer's pages keep the double they are checked against; staff's
/// commands, trials and the questions only staff ask go to the module.
pub struct Beside {
    pub employer: std::sync::Arc<super::group::GroupDouble>,
    pub staff: std::sync::Arc<GroupModule>,
}

impl Group for Beside {
    fn upload(&self, scheme: &str, file: &str, actor: &str, bytes: Vec<u8>) -> Asked<'_> {
        self.employer.upload(scheme, file, actor, bytes)
    }

    fn command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        self.employer.command(key, actor, command)
    }

    fn ask(&self, question: Value) -> Asked<'_> {
        if matches!(
            question["ask"].as_str(),
            Some("exceptions" | "mapping" | "possible_match" | "member_claim" | "member_offers")
        ) {
            self.staff.ask(question)
        } else {
            self.employer.ask(question)
        }
    }

    fn staff_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        self.staff.staff_command(key, actor, command)
    }

    fn try_mapping(&self, scheme: &str, trial: Value) -> Asked<'_> {
        self.staff.try_mapping(scheme, trial)
    }

    fn member_command(&self, key: &str, actor: &str, command: Value) -> Asked<'_> {
        self.staff.member_command(key, actor, command)
    }
}
