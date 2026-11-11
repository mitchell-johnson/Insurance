//! Proof that a region can reach the banks. An evacuation drill sends a
//! low-value file and a low-value API payment from one region through every
//! channel, and fails if any channel could not be reached. A region is
//! ready only while its endpoints are current and its last drill passed
//! recently.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::machine::{Event, PaymentsState, Tx};
use crate::model::{Drill, Id, Probe, Reject, Role};

/// Whether one region could submit to the banks today.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Readiness {
    pub region: String,
    pub ready: bool,
    /// The day of the last drill that passed from this region.
    pub last_passed: Option<NaiveDate>,
    pub problems: Vec<String>,
}

impl PaymentsState {
    pub(crate) fn start_drill(&mut self, tx: &mut Tx<'_>, id: Id, region: String) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if !self.cfg()?.rules.voter_regions.contains(&region) {
            return Err(Reject::new("unknown_region", format!("{region} is not a voter region")));
        }
        if self.drills.contains_key(&id) {
            return Err(Reject::new("duplicate_drill", format!("drill {id} already exists")));
        }
        self.drills.insert(
            id.clone(),
            Drill {
                region: region.clone(),
                started: tx.now,
                probes: BTreeMap::new(),
                passed: None,
            },
        );
        tx.emit(Event::DrillStarted { drill: id, region });
        Ok(())
    }

    fn open_drill(&mut self, id: &str) -> Result<&mut Drill, Reject> {
        match self.drills.get_mut(id) {
            Some(d) if d.passed.is_none() => Ok(d),
            Some(_) => Err(Reject::new("drill_closed", format!("drill {id} is closed"))),
            None => Err(Reject::new("unknown_drill", format!("there is no drill {id}"))),
        }
    }

    pub(crate) fn record_drill_probe(&mut self, tx: &mut Tx<'_>, id: &str, channel: Id, probe: Probe) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        if !self.cfg()?.channels.contains_key(&channel) {
            return Err(Reject::new("unknown_channel", format!("there is no channel {channel}")));
        }
        if probe.evidence_hash.is_empty() {
            return Err(Reject::new("no_evidence", "a probe names the evidence held for it"));
        }
        let ok = probe.ok;
        self.open_drill(id)?.probes.insert(channel.clone(), probe);
        tx.emit(Event::DrillProbed {
            drill: id.to_string(),
            channel,
            ok,
        });
        Ok(())
    }

    pub(crate) fn close_drill(&mut self, tx: &mut Tx<'_>, id: &str) -> Result<(), Reject> {
        tx.need(Role::Service)?;
        let channels: Vec<Id> = self.cfg()?.channels.keys().cloned().collect();
        let drill = self.open_drill(id)?;
        let unreached: Vec<Id> = channels.into_iter().filter(|c| !drill.probes.get(c).is_some_and(|p| p.ok)).collect();
        drill.passed = Some(unreached.is_empty());
        tx.emit(Event::DrillClosed {
            drill: id.to_string(),
            region: drill.region.clone(),
            passed: unreached.is_empty(),
            unreached,
        });
        Ok(())
    }

    /// Whether each voter region could submit a file and an urgent payment
    /// on `today`, and what stands in the way if it could not.
    pub fn readiness(&self, today: NaiveDate) -> Vec<Readiness> {
        let Some(cfg) = &self.config else { return Vec::new() };
        let gaps = cfg.provisioning_gaps(today);
        cfg.rules
            .voter_regions
            .iter()
            .map(|region| {
                let mut problems: Vec<String> = gaps.iter().filter(|g| g.contains(region.as_str())).cloned().collect();
                let last_passed = self
                    .drills
                    .values()
                    .filter(|d| &d.region == region && d.passed == Some(true))
                    .map(|d| d.started.date())
                    .max();
                match last_passed {
                    None => problems.push(format!("no drill has passed from {region}")),
                    Some(day) if (today - day).num_days() > i64::from(cfg.rules.drill_max_age_days) => {
                        problems.push(format!("the last drill from {region} passed on {day}"));
                    }
                    Some(_) => {}
                }
                Readiness {
                    region: region.clone(),
                    ready: problems.is_empty(),
                    last_passed,
                    problems,
                }
            })
            .collect()
    }
}
