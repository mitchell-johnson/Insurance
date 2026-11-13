//! Countersigned commands: one person prepares a command for another
//! module, a second person signs it, and only then does the portal send it.
//!
//! Some modules take a person's word for who approved a step (the
//! reinsurance module's `approved_by`) and keep no record of who asked. The
//! two people, and the rule that they differ, are kept here, in the
//! replicated channel state, so a restart or a second portal replica cannot
//! lose a half-approved command or let one person do both halves.

use std::collections::BTreeMap;

use ic_core::{PartyId, Reject};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::directory::Actor;
use crate::staff::{At, StaffState};

/// The most commands waiting for a signature at once, across modules.
pub const MAX_OPEN: usize = 1_000;

/// Who may prepare and who may sign a module's countersigned commands.
pub struct Rule {
    pub module: &'static str,
    pub prepare: &'static str,
    pub sign: &'static str,
    /// The commands of that module that go this way.
    pub commands: &'static [&'static str],
}

pub const RULES: [Rule; 2] = [
    Rule {
        module: crate::reinsurance::MODULE,
        prepare: crate::reinsurance::actions::OPERATE,
        sign: crate::reinsurance::actions::APPROVE,
        commands: &crate::reinsurance::COUNTERSIGNED,
    },
    Rule {
        module: crate::underwriting::MODULE,
        prepare: crate::underwriting::actions::SENIOR,
        sign: crate::underwriting::actions::SENIOR,
        commands: &crate::underwriting::COUNTERSIGNED,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    pub module: String,
    /// The command as it will be sent, less the signer.
    pub command: Value,
    pub reason: String,
    pub by: PartyId,
    pub at: At,
    /// Set when a second person signs; the portal then sends it.
    pub signed_by: Option<PartyId>,
    pub signed_at: Option<At>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountersignCommand {
    Prepare {
        id: String,
        module: String,
        command: Value,
        reason: String,
    },
    /// Sign as the second person. Signing again, as the same signer, is
    /// how a send that was interrupted is finished.
    Sign { id: String },
    /// The person who prepared it takes it back before it is signed.
    Withdraw { id: String },
    /// The module has answered: it is done, or refused and finished with.
    Settle { id: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Countersign {
    pub open: BTreeMap<String, Pending>,
}

fn reject(code: &str, message: impl Into<String>) -> Reject {
    Reject::new(code, message)
}

fn rule(module: &str) -> Result<&'static Rule, Reject> {
    RULES
        .iter()
        .find(|r| r.module == module)
        .ok_or_else(|| reject("unknown_module", format!("nothing of {module} is countersigned")))
}

/// The name of a command given as one name with its fields.
pub fn command_name(command: &Value) -> Option<&str> {
    match command {
        Value::Object(fields) if fields.len() == 1 => fields.keys().next().map(String::as_str),
        _ => None,
    }
}

impl Countersign {
    /// Apply one command. Returns the pending command as it now stands.
    pub fn apply(&mut self, staff: &StaffState, actor: &Actor, at: At, command: CountersignCommand) -> Result<Pending, Reject> {
        let Actor::Staff(person) = actor else {
            return Err(reject("forbidden", "countersigned commands come from staff"));
        };
        match command {
            CountersignCommand::Prepare { id, module, command, reason } => {
                let rule = rule(&module)?;
                if !staff.holds_action(person, rule.prepare, at) {
                    return Err(reject("forbidden", format!("preparing this needs {}", rule.prepare)));
                }
                let name = command_name(&command).ok_or_else(|| reject("bad_command", "a command is one name with its fields"))?;
                if !rule.commands.contains(&name) {
                    return Err(reject("bad_command", format!("{name} is not countersigned here")));
                }
                if reason.trim().is_empty() {
                    return Err(reject("reason_required", "say why"));
                }
                if self.open.contains_key(&id) {
                    return Err(reject("exists", format!("{id} is already waiting")));
                }
                if self.open.len() >= MAX_OPEN {
                    return Err(reject("full", "too many commands are waiting for a signature"));
                }
                let pending = Pending {
                    id: id.clone(),
                    module,
                    command,
                    reason: reason.trim().to_string(),
                    by: person.clone(),
                    at,
                    signed_by: None,
                    signed_at: None,
                };
                self.open.insert(id, pending.clone());
                Ok(pending)
            }
            CountersignCommand::Sign { id } => {
                let pending = self.open.get_mut(&id).ok_or_else(|| reject("not_found", format!("{id} is not waiting")))?;
                let rule = rule(&pending.module)?;
                if &pending.by == person {
                    return Err(reject("separation", "someone else signs what you prepared"));
                }
                if !staff.holds_action(person, rule.sign, at) {
                    return Err(reject("forbidden", format!("signing this needs {}", rule.sign)));
                }
                match &pending.signed_by {
                    Some(signer) if signer != person => return Err(reject("signed", format!("{id} is already signed by {signer}"))),
                    Some(_) => {}
                    None => {
                        pending.signed_by = Some(person.clone());
                        pending.signed_at = Some(at);
                    }
                }
                Ok(pending.clone())
            }
            CountersignCommand::Withdraw { id } => {
                let pending = self.open.get(&id).ok_or_else(|| reject("not_found", format!("{id} is not waiting")))?;
                if &pending.by != person {
                    return Err(reject("forbidden", "only the person who prepared it withdraws it"));
                }
                if pending.signed_by.is_some() {
                    return Err(reject("signed", "it is signed and being sent"));
                }
                Ok(self.open.remove(&id).expect("checked above"))
            }
            CountersignCommand::Settle { id } => {
                let pending = self.open.get(&id).ok_or_else(|| reject("not_found", format!("{id} is not waiting")))?;
                if pending.signed_by.as_ref() != Some(person) {
                    return Err(reject("forbidden", "only the signer settles it"));
                }
                Ok(self.open.remove(&id).expect("checked above"))
            }
        }
    }

    /// What is waiting for one module.
    pub fn waiting(&self, module: &str) -> Vec<&Pending> {
        self.open.values().filter(|p| p.module == module).collect()
    }
}
