//! A scheme's health menu in force: the covers each member and their
//! dependants hold in the core, and who pays for each.
//!
//! The core holds a life's chosen covers as one whole set from a date
//! (`SetMemberCovers`), so sending the same set again changes nothing. A
//! member is on the menu's default plan from the day they join until they
//! choose; a dependant is a life of its own on the scheme policy, joined and
//! removed with `ApplyMembership`, that holds only the covers chosen for it
//! and leaves and returns with its member in the core. The employer pays for
//! what the plan says it pays for; the member pays for upgrades and, unless
//! the scheme says otherwise, for their dependants.

use std::collections::BTreeSet;

use chrono::{Days, NaiveDate};
use ic_core::command::{LifeSpec, MemberChange};
use ic_core::{Command, LifeFactors, LifeId, LifeKind, Reject, SetMemberCovers};

use crate::command::DependantSpec;
use crate::event::GroupEvent;
use crate::model::{Dependant, HealthChoice};
use crate::state::{GroupState, Scheme};
use crate::terms::{HealthMenu, HealthTier};
use crate::work::{After, Item, Work};

/// At most this many dependants on one member's plan.
pub const MAX_DEPENDANTS: usize = 12;

/// What a member asked for.
pub(crate) struct Choice {
    pub employee_id: String,
    pub plan: String,
    pub tier: HealthTier,
    pub effective: NaiveDate,
    pub dependants: Vec<DependantSpec>,
}

fn plain(key: &str) -> bool {
    !key.is_empty() && key.len() <= 32 && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl GroupState {
    /// A member's choice: check it, then send the dependants who join or
    /// come off the plan and every life's covers, in that order, on the day.
    pub(crate) fn choose_health_plan(&mut self, s: &mut Scheme, today: NaiveDate, c: Choice) -> Result<(), Reject> {
        let menu = s
            .terms
            .health_menu
            .clone()
            .ok_or_else(|| Reject::new("no_menu", "this scheme has no health menu"))?;
        if menu.plan(&c.plan).is_none() {
            return Err(Reject::new("unknown_plan", format!("{} is not on the scheme's health menu", c.plan)));
        }
        if !menu.tiers.contains(&c.tier) {
            return Err(Reject::new("tier_not_offered", "the scheme does not offer that tier"));
        }
        let fits = match c.tier {
            HealthTier::Single => c.dependants.is_empty(),
            HealthTier::Couple => c.dependants.len() == 1,
            HealthTier::Family => !c.dependants.is_empty() && c.dependants.len() <= MAX_DEPENDANTS,
        };
        if !fits {
            return Err(Reject::new(
                "dependants_for_tier",
                format!("single covers nobody else, couple one person and family one to {MAX_DEPENDANTS}"),
            ));
        }
        let keys: BTreeSet<&str> = c.dependants.iter().map(|d| d.key.as_str()).collect();
        if keys.len() != c.dependants.len() || !c.dependants.iter().all(|d| plain(&d.key)) {
            return Err(Reject::new("invalid_dependant", "name each dependant once, in letters, digits and dashes"));
        }
        if c.dependants.iter().any(|d| d.date_of_birth > c.effective) {
            return Err(Reject::new("invalid_dependant", "a dependant is born by the day the plan covers them"));
        }
        let earliest = today.checked_sub_days(Days::new(u64::from(menu.choice_backdating_days))).unwrap_or(today);
        let member = s
            .membership
            .active(&c.employee_id)
            .cloned()
            .ok_or_else(|| Reject::new("unknown_member", format!("{} is not a member", c.employee_id)))?;
        if c.effective < earliest.max(member.joined) {
            return Err(Reject::new("backdating_limit", "a choice cannot take effect that far back"));
        }
        if self.health_waiting(s, &c.employee_id) {
            return Err(Reject::new("choice_in_progress", "the member's last choice is still being put in place"));
        }

        let held: BTreeSet<&str> = member.dependants.iter().map(|d| d.key.as_str()).collect();
        let joined: Vec<Dependant> = c
            .dependants
            .iter()
            .filter(|d| !held.contains(d.key.as_str()))
            .map(|d| Dependant {
                key: d.key.clone(),
                life: LifeId(format!("{}+{}", member.life.0, d.key)),
                date_of_birth: d.date_of_birth,
                joined: c.effective,
            })
            .collect();
        let left: Vec<&Dependant> = member.dependants.iter().filter(|d| !keys.contains(d.key.as_str())).collect();
        let mut work = Vec::new();
        if !joined.is_empty() || !left.is_empty() {
            let mut changes: Vec<MemberChange> = joined
                .iter()
                .map(|d| MemberChange::Joined {
                    life: LifeSpec {
                        id: d.life.clone(),
                        kind: LifeKind::Dependant {
                            of: member.life.clone(),
                            party: None,
                        },
                        factors: LifeFactors {
                            date_of_birth: Some(d.date_of_birth),
                            ..LifeFactors::default()
                        },
                    },
                    effective: c.effective,
                })
                .collect();
            changes.extend(left.iter().map(|d| MemberChange::Left {
                life: d.life.clone(),
                effective: c.effective,
            }));
            work.push((
                Command::ApplyMembership {
                    policy: s.policy.clone(),
                    changes,
                },
                After::Health {
                    employee_id: c.employee_id.clone(),
                    joined: joined.clone(),
                    left: left.iter().map(|d| d.key.clone()).collect(),
                    chosen: None,
                },
            ));
        }
        let reference = format!("grp-health:{}:{}:{}", s.id, c.employee_id, c.effective);
        let mut lives = vec![(member.life.clone(), false)];
        lives.extend(
            member
                .dependants
                .iter()
                .filter(|d| keys.contains(d.key.as_str()))
                .map(|d| (d.life.clone(), true)),
        );
        lives.extend(joined.iter().map(|d| (d.life.clone(), true)));
        let count = lives.len();
        for (n, (life, dependant)) in lives.into_iter().enumerate() {
            let chosen = (n + 1 == count).then(|| HealthChoice {
                plan: c.plan.clone(),
                tier: c.tier,
                effective: c.effective,
            });
            work.push((
                set_covers(s, life, menu.covers(&c.plan, dependant), c.effective, &reference),
                After::Health {
                    employee_id: c.employee_id.clone(),
                    joined: vec![],
                    left: vec![],
                    chosen,
                },
            ));
        }
        for (command, then) in work {
            self.send_on(s, today, c.effective, command, then);
        }
        self.pump(s);
        Ok(())
    }

    /// A joiner is on the default plan from the day they join.
    pub(crate) fn enrol_default(&mut self, s: &mut Scheme, employee_id: &str, effective: NaiveDate) {
        let Some(menu) = &s.terms.health_menu else { return };
        let Some(member) = s.membership.active(employee_id) else { return };
        let covers = menu.covers(&menu.default_plan, false);
        let reference = format!("grp-health:{}:{employee_id}:{effective}", s.id);
        let command = set_covers(s, member.life.clone(), covers, effective, &reference);
        let then = After::Health {
            employee_id: employee_id.to_string(),
            joined: vec![],
            left: vec![],
            chosen: None,
        };
        let today = self.today();
        self.send_on(s, today, effective, command, then);
    }

    /// The menu changed: every member's and dependant's covers are sent
    /// again from the date as the new menu has them. A plan no longer on
    /// the menu is the default plan; no menu is no chosen cover.
    pub(crate) fn menu_changed(&mut self, s: &mut Scheme, before: Option<&HealthMenu>, effective: NaiveDate) {
        if before == s.terms.health_menu.as_ref() {
            return;
        }
        let today = self.today();
        let members: Vec<_> = s.membership.active_members().cloned().collect();
        for m in members {
            let plan = s.terms.health_menu.as_ref().map(|menu| {
                let chosen = m.health.as_ref().map(|h| h.plan.clone()).filter(|p| menu.plan(p).is_some());
                (menu.clone(), chosen.unwrap_or_else(|| menu.default_plan.clone()))
            });
            let reference = format!("grp-health:{}:{}:{effective}", s.id, m.employee_id);
            let lives = std::iter::once((m.life.clone(), false)).chain(m.dependants.iter().map(|d| (d.life.clone(), true)));
            for (life, dependant) in lives {
                let covers = plan.as_ref().map(|(menu, p)| menu.covers(p, dependant)).unwrap_or_default();
                let command = set_covers(s, life, covers, effective, &reference);
                let then = After::Health {
                    employee_id: m.employee_id.clone(),
                    joined: vec![],
                    left: vec![],
                    chosen: None,
                };
                self.send_on(s, today, effective, command, then);
            }
        }
    }

    /// The core accepted a step of a member's health plan.
    pub(crate) fn health_step(&mut self, s: &mut Scheme, employee_id: &str, joined: Vec<Dependant>, left: Vec<String>, chosen: Option<HealthChoice>) {
        let Some(m) = s.membership.members.get_mut(employee_id) else { return };
        m.dependants.retain(|d| !left.contains(&d.key));
        m.dependants.extend(joined);
        let Some(choice) = chosen else { return };
        m.health = Some(choice.clone());
        let life = m.life.clone();
        self.emit(GroupEvent::HealthPlanChosen {
            scheme: s.id.clone(),
            life,
            plan: choice.plan,
            tier: choice.tier,
            effective: choice.effective,
        });
    }

    /// The core refused a step: the rest of the choice is not sent, and
    /// staff and the member's channel are told.
    pub(crate) fn health_refused(&mut self, s: &mut Scheme, employee_id: &str, code: &str) {
        let ours = |i: &Item| matches!(&i.work, Work::Core { then: After::Health { employee_id: e, .. }, .. } if e == employee_id);
        s.queue.retain(|i| !ours(i));
        s.scheduled.retain(|_, i| !ours(i));
        let life = s.membership.members.get(employee_id).map(|m| m.life.clone());
        if let Some(life) = life {
            self.emit(GroupEvent::HealthPlanRefused {
                scheme: s.id.clone(),
                life,
                code: code.to_string(),
            });
        }
    }

    fn health_waiting(&self, s: &Scheme, employee_id: &str) -> bool {
        let ours = |i: &Item| matches!(&i.work, Work::Core { then: After::Health { employee_id: e, .. }, .. } if e == employee_id);
        s.queue.iter().any(ours) || s.scheduled.values().any(ours) || s.in_flight.as_ref().is_some_and(|f| f.items.iter().any(ours))
    }

    /// Queue a command for the core now, or on its day if that is later:
    /// the core takes no chosen cover dated ahead.
    fn send_on(&mut self, s: &mut Scheme, today: NaiveDate, effective: NaiveDate, command: Command, then: After) {
        let work = Work::Core {
            group: s.group.clone(),
            command: Box::new(command),
            then,
        };
        if effective > today {
            s.next_item += 1;
            s.scheduled.insert(
                (effective, s.next_item),
                Item {
                    id: s.next_item,
                    file: None,
                    work,
                },
            );
        } else {
            self.enqueue(s, None, work);
        }
    }
}

fn set_covers(s: &Scheme, life: LifeId, covers: Vec<ic_core::ChosenCover>, effective: NaiveDate, reference: &str) -> Command {
    Command::SetMemberCovers(Box::new(SetMemberCovers {
        policy: s.policy.clone(),
        life,
        covers,
        effective,
        reference: reference.to_string(),
    }))
}
