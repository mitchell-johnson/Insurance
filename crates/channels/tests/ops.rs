//! Region health as the operations console and the status page see it.

use std::collections::BTreeMap;

use ic_channels::ops::{assess, Level, Regions, StatusPage};
use ic_raft::NodeStatus;
use serde_json::json;

/// One node's view of one partition.
fn status(id: u64, partition: u32, role: &str) -> NodeStatus {
    serde_json::from_value(json!({
        "id": id, "partition": partition, "role": role, "leader": null, "term": 1,
        "last_log_index": 1, "last_applied": 1, "business_date": null, "state_hash": "", "ledger_head": "",
        "ledger_entries": 0, "policies": 0, "voters": [], "first_log_index": null,
        "backlog": { "events": 0, "messages": 0, "journal": 0 },
    }))
    .unwrap()
}

fn regions() -> Regions {
    Regions::parse("auckland=a1:7000, a2:7000 ; sydney=s1:7000,s2:7000; melbourne=m1:7000", "auckland").unwrap()
}

/// Every node's answer, with the two partitions led by `leader`.
fn observed(down: &[&str], leader: &str) -> BTreeMap<String, Option<Vec<NodeStatus>>> {
    ["a1:7000", "a2:7000", "s1:7000", "s2:7000", "m1:7000"]
        .iter()
        .enumerate()
        .map(|(i, addr)| {
            let role = if *addr == leader { "leader" } else { "follower" };
            let answer = (!down.contains(addr)).then(|| vec![status(i as u64, 0, role), status(i as u64, 1, role)]);
            (addr.to_string(), answer)
        })
        .collect()
}

/// Purpose: the region map is read from configuration, and a map that cannot be right is refused at start.
/// Why it matters: a status page built on a wrong region map tells customers the wrong thing during an incident.
#[test]
fn the_region_map_is_parsed_and_checked() {
    let r = regions();
    assert_eq!(r.nodes["auckland"], ["a1:7000", "a2:7000"]);
    assert_eq!(r.nodes.len(), 3);
    assert!(Regions::parse("auckland=a1:7000", "sydney").unwrap_err().contains("home region"));
    assert!(Regions::parse("auckland", "auckland").unwrap_err().contains("region=host:port"));
    assert!(Regions::parse("auckland=", "auckland").is_err());
    assert!(Regions::parse("=a1:7000", "auckland").is_err());
    let one = Regions::single(vec!["n:1".into()]);
    assert_eq!((one.home.as_str(), one.nodes["local"].len()), ("local", 1));
}

/// Purpose: with every node up and the home region leading, the report is at home and the page says all is normal.
/// Why it matters: the normal state must read as normal; a status page that cries wolf is ignored when it matters.
#[test]
fn all_well() {
    let report = assess(&regions(), &observed(&[], "a1:7000"));
    assert!(report.quorum && report.at_home);
    assert_eq!((report.partitions, report.partitions_with_leader), (2, 2));
    let auckland = report.regions.iter().find(|r| r.home).unwrap();
    assert_eq!((auckland.name.as_str(), auckland.reachable, auckland.leaders), ("auckland", 2, 2));
    let page = StatusPage::from(&report);
    assert_eq!((page.level, page.detail.len()), (Level::Operational, 0));
}

/// Purpose: when the home region is lost and another region leads, the service is degraded, not down; and after the home region returns but before leaders move back, the page says we will move back by ourselves.
/// Why it matters: addendum entry 6: three of five voters remain and nothing is lost; customers should be told it is slower, not that it is broken, and operators should see that fail-back is still to come.
#[test]
fn home_region_lost_then_returned() {
    let report = assess(&regions(), &observed(&["a1:7000", "a2:7000"], "s1:7000"));
    assert!(report.quorum && !report.at_home);
    assert_eq!(report.partitions_with_leader, 2);
    assert_eq!(report.regions.iter().find(|r| r.name == "sydney").unwrap().leaders, 2);
    let page = StatusPage::from(&report);
    assert_eq!(page.level, Level::Degraded);
    assert!(page.detail[1].contains("One of our data centres is unavailable"));

    // Auckland is back as followers; Sydney still leads until fail-back.
    let report = assess(&regions(), &observed(&[], "s1:7000"));
    assert!(!report.at_home);
    let page = StatusPage::from(&report);
    assert_eq!(page.level, Level::Degraded);
    assert!(page.detail[1].contains("move back by ourselves"));

    // One home node down while the other still leads is not at home, and is said to be slower.
    let report = assess(&regions(), &observed(&["a2:7000"], "a1:7000"));
    assert!(!report.at_home && report.quorum);
}

/// Purpose: without a majority, with a partition that has no leader, or with nothing answering, the page says services are unavailable and that cover is not affected.
/// Why it matters: an outage message must never leave a customer wondering whether they are still insured.
#[test]
fn outages_are_said_plainly() {
    // Two of five answer: no quorum.
    let report = assess(&regions(), &observed(&["a1:7000", "a2:7000", "s1:7000"], "s2:7000"));
    assert!(!report.quorum);
    assert_eq!(StatusPage::from(&report).level, Level::Outage);

    // A majority answers but nobody leads.
    let report = assess(&regions(), &observed(&[], "nobody"));
    assert_eq!((report.quorum, report.partitions_with_leader), (true, 0));
    let page = StatusPage::from(&report);
    assert_eq!(page.level, Level::Outage);
    assert!(page.detail.iter().any(|d| d.contains("Your cover is not affected")));

    // Nothing answers, and a node the map does not know is ignored.
    let mut nothing = observed(&["a1:7000", "a2:7000", "s1:7000", "s2:7000", "m1:7000"], "x");
    nothing.insert("stray:1".into(), Some(vec![status(9, 0, "leader")]));
    let report = assess(&regions(), &nothing);
    assert_eq!((report.partitions, report.quorum, report.at_home), (0, false, false));
    assert_eq!(StatusPage::from(&report).level, Level::Outage);
    // Two nodes claiming the same partition count as one leader.
    let mut split = observed(&[], "a1:7000");
    split.insert("s1:7000".into(), Some(vec![status(2, 0, "leader")]));
    assert_eq!(assess(&regions(), &split).partitions_with_leader, 2);
}

/// Purpose: with two home regions sharing the leadership and a witness's region that leads
/// nothing, the report is at home while each home region leads its share and every home node
/// answers; losing either home region is degraded, not down, and a home region that is named
/// but not in the map is refused.
/// Why it matters: this is the shipped placement (Sydney and Melbourne share the partitions,
/// a witness in a third region). A status page that took Melbourne's share of the leaders for
/// a fail-over would tell every customer the service is degraded on a normal day.
#[test]
fn two_home_regions_share_the_leaders() {
    let shared = Regions::parse(
        "australiaeast=s1:7000,s2:7000;australiasoutheast=m1:7000,m2:7000;australiacentral=w1:7000",
        "australiaeast, australiasoutheast",
    )
    .unwrap();
    assert_eq!(shared.home, "australiaeast");
    assert_eq!(shared.homes.len(), 2);
    let nodes = ["s1:7000", "s2:7000", "m1:7000", "m2:7000", "w1:7000"];
    let seen = |down: &[&str], leaders: &[(&str, u32)]| -> BTreeMap<String, Option<Vec<NodeStatus>>> {
        nodes
            .iter()
            .enumerate()
            .map(|(i, addr)| {
                let rows = (0..2u32)
                    .map(|p| {
                        let role = if leaders.contains(&(*addr, p)) { "leader" } else { "follower" };
                        status(i as u64, p, role)
                    })
                    .collect();
                (addr.to_string(), (!down.contains(addr)).then_some(rows))
            })
            .collect()
    };
    let normal = assess(&shared, &seen(&[], &[("s1:7000", 0), ("m1:7000", 1)]));
    assert!(normal.at_home && normal.quorum);
    assert_eq!(StatusPage::from(&normal).level, Level::Operational);
    let witness = normal.regions.iter().find(|r| r.name == "australiacentral").unwrap();
    assert!(!witness.home);

    let sydney_lost = assess(&shared, &seen(&["s1:7000", "s2:7000"], &[("m1:7000", 0), ("m1:7000", 1)]));
    assert!(sydney_lost.quorum && !sydney_lost.at_home);
    let page = StatusPage::from(&sydney_lost);
    assert_eq!(page.level, Level::Degraded);
    assert!(page.detail[1].contains("One of our data centres is unavailable"));
    assert!(Regions::parse("australiaeast=s1:7000", "australiaeast,westus").unwrap_err().contains("westus"));
    assert!(Regions::parse("australiaeast=s1:7000", " , ").unwrap_err().contains("home region"));
}
