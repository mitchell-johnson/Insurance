//! Conduct cases: licence events, breaches, incidents and adviser patterns.
//!
//! A conduct case is owned by the conduct officer. It carries the affected
//! population, drives the communication to each customer, a sampled review
//! of advice files, the notification to the regulator and the regulator's
//! requests for information.

use std::collections::{BTreeMap, BTreeSet};

use ic_core::{PartyId, PolicyId, Reject};
use sha2::{Digest, Sha256};

use crate::command::{Command, Event};
use crate::model::*;
use crate::state::{days, level, reject, ComplaintsState, Ctx, MAX_CHUNK};

/// The ID of the conduct case a licence event opens. Derived from the
/// event, so the event opens one case however often it is delivered.
pub fn licence_case_id(licence_event: &str) -> String {
    format!("licence:{licence_event}")
}

impl ComplaintsState {
    pub(crate) fn dispatch_conduct(&mut self, command: Command, ctx: &mut Ctx) -> Result<(), Reject> {
        match command {
            Command::OpenConductCase {
                case,
                kind,
                owner,
                summary_document,
                notify_regulator,
            } => self.open_conduct_case(case, kind, owner, summary_document, notify_regulator, ctx),
            Command::AttachPopulation {
                case,
                customers,
                advisers,
                policies,
            } => self.attach_population(&case, customers, advisers, policies, ctx),
            Command::CompletePopulation { case, customers, policies } => {
                let c = self.case_mut(&case)?;
                if c.population_complete {
                    return reject("already_complete", "the population is already complete");
                }
                if c.customers.len() as u32 != customers || c.policies.len() as u32 != policies {
                    return reject(
                        "count_mismatch",
                        format!("{} customers and {} policies are attached", c.customers.len(), c.policies.len()),
                    );
                }
                c.population_complete = true;
                ctx.events.push(Event::PopulationComplete { case, customers });
                Ok(())
            }
            Command::RecordCustomerNotified { case, customer, document } => {
                let today = ctx.today;
                let c = self.case_mut(&case)?;
                match c.customers.get_mut(&customer) {
                    Some(contact @ Contact::Outstanding) if !document.is_empty() => *contact = Contact::Notified { document, on: today },
                    _ => return reject("not_outstanding", "that customer is not waiting to be told, or no document was named"),
                }
                Ok(())
            }
            Command::ExcludeCustomer { case, customer, reason } => {
                let c = self.case_mut(&case)?;
                match c.customers.get_mut(&customer) {
                    Some(contact @ Contact::Outstanding) if !reason.is_empty() => *contact = Contact::Excluded { reason },
                    _ => return reject("not_outstanding", "that customer is not waiting to be told, or no reason was given"),
                }
                Ok(())
            }
            Command::DrawSample { case, size } => self.draw_sample(&case, size, ctx),
            Command::RecordFileReview { case, policy, finding } => self.review_file(&case, policy, finding, ctx),
            Command::RecordRegulatorNotification { case, reference, document } => {
                let today = ctx.today;
                let c = self.case_mut(&case)?;
                match c.regulator.as_mut().filter(|r| r.notified_on.is_none()) {
                    Some(r) if !reference.is_empty() && !document.is_empty() => {
                        r.reference = Some(reference.clone());
                        r.document = Some(document);
                        r.notified_on = Some(today);
                    }
                    _ => return reject("not_due", "no notification to the regulator is outstanding, or it was not identified"),
                }
                ctx.events.push(Event::RegulatorNotified { case, reference });
                Ok(())
            }
            Command::RecordRegulatorRequest { case, request, due, document } => {
                let today = ctx.today;
                let c = self.case_mut(&case)?;
                if request.is_empty() || document.is_empty() || due < today || c.requests.contains_key(&request) {
                    return reject(
                        "invalid",
                        "a request is recorded once, with the document received and a date that has not passed",
                    );
                }
                c.requests.insert(
                    request.clone(),
                    RegulatorRequest {
                        received_on: today,
                        due,
                        document,
                        response: None,
                        responded_on: None,
                    },
                );
                ctx.events.push(Event::RegulatorRequestReceived { case, request, due });
                Ok(())
            }
            Command::RespondToRegulator { case, request, document } => {
                let today = ctx.today;
                let c = self.case_mut(&case)?;
                match c.requests.get_mut(&request).filter(|r| r.response.is_none()) {
                    Some(r) if !document.is_empty() => {
                        r.response = Some(document);
                        r.responded_on = Some(today);
                    }
                    _ => return reject("not_outstanding", "no such request is waiting for a response, or no document was named"),
                }
                c.escalated.remove(&format!("request:{request}"));
                ctx.events.push(Event::RegulatorAnswered { case, request });
                Ok(())
            }
            Command::LinkComplaint { case, complaint } => {
                if !self.complaints.contains_key(&complaint) {
                    return reject("unknown_complaint", format!("there is no complaint {complaint}"));
                }
                if !self.case_mut(&case)?.complaints.insert(complaint) {
                    return reject("exists", "that complaint is already linked");
                }
                Ok(())
            }
            Command::CloseConductCase { case, findings_document } => self.close_conduct_case(&case, findings_document, ctx),
            // Every other command is dispatched before this is reached.
            other => reject("invalid", format!("{} is not a conduct command", other.name())),
        }
    }

    fn case_mut(&mut self, id: &str) -> Result<&mut ConductCase, Reject> {
        match self.conduct.get_mut(id) {
            None => reject("unknown_case", format!("there is no conduct case {id}")),
            Some(c) if c.closed_on.is_some() => reject("closed", "the conduct case is closed"),
            Some(c) => Ok(c),
        }
    }

    fn open_conduct_case(
        &mut self,
        case: String,
        kind: ConductKind,
        owner: String,
        summary_document: Option<String>,
        notify_regulator: bool,
        ctx: &mut Ctx,
    ) -> Result<(), Reject> {
        let Some(timers) = self.timers(self.timers_version).cloned() else {
            return reject("no_timers", "no timer configuration is active");
        };
        if case.is_empty() || owner.is_empty() {
            return reject("invalid", "a conduct case has an id and an owner");
        }
        let licence_event = match &kind {
            ConductKind::LicenceEvent { licence_event, status, .. } => {
                if case != licence_case_id(licence_event) || status.is_empty() {
                    return reject("invalid", "a licence event opens the case named after it");
                }
                Some(licence_event.clone())
            }
            // The commission and adviser module reports licence events;
            // only they are opened from outside the conduct function.
            _ if ctx.caller == "distribution" => return reject("not_allowed", "distribution opens conduct cases for licence events only"),
            ConductKind::Breach | ConductKind::Incident if ctx.caller != "conduct" => {
                return reject("not_allowed", "a breach or incident is recorded by the conduct function");
            }
            _ => None,
        };
        if let Some(existing) = self.conduct.get(&case) {
            // The same licence event delivered again changes nothing.
            return if existing.kind == kind {
                Ok(())
            } else {
                reject("exists", "a conduct case with that id is already open")
            };
        }
        self.conduct.insert(
            case.clone(),
            ConductCase {
                id: case.clone(),
                kind,
                owner: owner.clone(),
                opened_on: ctx.today,
                opened_by: ctx.actor.clone(),
                summary_document,
                customers: BTreeMap::new(),
                advisers: BTreeSet::new(),
                policies: BTreeSet::new(),
                population_complete: false,
                sample: BTreeMap::new(),
                regulator: notify_regulator.then(|| RegulatorNotification {
                    due: ctx.today + days(timers.regulator_notification_days),
                    reference: None,
                    document: None,
                    notified_on: None,
                }),
                requests: BTreeMap::new(),
                complaints: BTreeSet::new(),
                escalated: BTreeMap::new(),
                timers_version: timers.version,
                closed_on: None,
                findings_document: None,
            },
        );
        self.open_conduct.insert(case.clone());
        ctx.events.push(Event::ConductCaseOpened { case, owner, licence_event });
        Ok(())
    }

    fn attach_population(&mut self, case: &str, customers: Vec<PartyId>, advisers: Vec<PartyId>, policies: Vec<PolicyId>, ctx: &mut Ctx) -> Result<(), Reject> {
        if customers.len() + advisers.len() + policies.len() > MAX_CHUNK {
            return reject("invalid", format!("at most {MAX_CHUNK} at a time"));
        }
        let c = self.case_mut(case)?;
        if c.population_complete {
            return reject("already_complete", "the population is already complete");
        }
        // A chunk delivered twice attaches nothing new.
        for customer in customers {
            c.customers.entry(customer).or_insert(Contact::Outstanding);
        }
        c.advisers.extend(advisers);
        c.policies.extend(policies);
        ctx.events.push(Event::PopulationAttached {
            case: case.to_string(),
            customers: c.customers.len() as u32,
            policies: c.policies.len() as u32,
        });
        Ok(())
    }

    fn draw_sample(&mut self, case: &str, size: u32, ctx: &mut Ctx) -> Result<(), Reject> {
        let c = self.case_mut(case)?;
        if !c.population_complete {
            return reject("population_incomplete", "the sample is drawn from the whole population");
        }
        if !c.sample.is_empty() || size == 0 || c.policies.is_empty() {
            return reject("invalid", "a sample of at least one file is drawn once, from a population with advice files");
        }
        // Ordered by a hash of the case and the policy: anyone can repeat
        // the draw and nobody could have steered it.
        let mut order: Vec<(String, &PolicyId)> = c
            .policies
            .iter()
            .map(|p| (hex::encode(Sha256::digest(format!("{case}|{p}").as_bytes())), p))
            .collect();
        order.sort();
        c.sample = order.into_iter().take(size as usize).map(|(_, p)| (p.clone(), None)).collect();
        ctx.events.push(Event::SampleDrawn {
            case: case.to_string(),
            files: c.sample.len() as u32,
        });
        Ok(())
    }

    fn review_file(&mut self, case: &str, policy: PolicyId, finding: ReviewFinding, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let actor = ctx.actor.clone();
        let c = self.case_mut(case)?;
        if c.advisers.iter().any(|a| a.to_string() == actor) {
            return reject("separation", "an adviser whose files are under review does not review them");
        }
        let remediation_case = match &finding {
            ReviewFinding::RemediationNeeded { remediation_case } if remediation_case.is_empty() => {
                return reject("invalid", "a file that needs putting right names the remediation case");
            }
            ReviewFinding::RemediationNeeded { remediation_case } => Some(remediation_case.clone()),
            ReviewFinding::Satisfactory => None,
        };
        match c.sample.get_mut(&policy) {
            Some(slot @ None) => {
                *slot = Some(FileReview {
                    reviewer: actor,
                    on: today,
                    finding,
                })
            }
            Some(Some(_)) => return reject("already_reviewed", "that file is already reviewed"),
            None => return reject("not_sampled", "that file is not in the sample"),
        }
        ctx.events.push(Event::FileReviewed {
            case: case.to_string(),
            policy,
            remediation_case,
        });
        Ok(())
    }

    fn close_conduct_case(&mut self, case: &str, findings_document: String, ctx: &mut Ctx) -> Result<(), Reject> {
        let today = ctx.today;
        let c = self.case_mut(case)?;
        if findings_document.is_empty() {
            return reject("invalid", "a conduct case is closed with its findings");
        }
        let has_population = !c.customers.is_empty() || !c.policies.is_empty() || matches!(c.kind, ConductKind::LicenceEvent { .. });
        if has_population && !c.population_complete {
            return reject("population_incomplete", "the affected population is not complete");
        }
        if c.customers.values().any(|contact| *contact == Contact::Outstanding) {
            return reject("customers_outstanding", "some affected customers have not been told");
        }
        // A licence event with advice files in scope always has them sampled.
        let sample_owed = matches!(c.kind, ConductKind::LicenceEvent { .. }) && !c.policies.is_empty() && c.sample.is_empty();
        if sample_owed || c.sample.values().any(Option::is_none) {
            return reject("review_outstanding", "the review of advice files is not finished");
        }
        if c.regulator.as_ref().is_some_and(|r| r.notified_on.is_none()) {
            return reject("regulator_outstanding", "the regulator has not been notified");
        }
        if c.requests.values().any(|r| r.response.is_none()) {
            return reject("regulator_outstanding", "a request from the regulator is unanswered");
        }
        c.closed_on = Some(today);
        c.findings_document = Some(findings_document);
        self.open_conduct.remove(case);
        ctx.events.push(Event::ConductCaseClosed { case: case.to_string() });
        Ok(())
    }

    pub(crate) fn tick_conduct(&mut self, ctx: &mut Ctx) {
        let today = ctx.today;
        let ids: Vec<String> = self.open_conduct.iter().take(MAX_CHUNK).cloned().collect();
        for id in ids {
            let c = &self.conduct[&id];
            let Some(timers) = self.timers(c.timers_version).cloned() else { continue };
            let mut running: Vec<(String, Timer, chrono::NaiveDate)> = vec![];
            if let Some(r) = c.regulator.as_ref().filter(|r| r.notified_on.is_none()) {
                running.push(("notification".to_string(), Timer::RegulatorNotification, r.due));
            }
            for (request, r) in c.requests.iter().filter(|(_, r)| r.response.is_none()) {
                running.push((format!("request:{request}"), Timer::RegulatorRequest, r.due));
            }
            let raised: Vec<_> = running
                .into_iter()
                .filter_map(|(key, timer, due)| {
                    let reached = level(&timers, due, today)?;
                    c.escalated.get(&key).is_none_or(|had| *had < reached).then_some((key, timer, reached, due))
                })
                .collect();
            let c = self.conduct.get_mut(&id).expect("listed as open");
            for (key, timer, reached, due) in raised {
                c.escalated.insert(key, reached);
                ctx.events.push(Event::TimerEscalated {
                    subject: id.clone(),
                    timer,
                    level: reached,
                    due,
                });
            }
        }
    }
}
