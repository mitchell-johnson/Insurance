//! Which business date is due to be closed.
//!
//! A business date closes at the midnight that ends it in New Zealand. The
//! scheduled job asks, each time it runs, which date the ledger is on and
//! whether that date's midnight has passed. If it has, the date is closed,
//! and the question is asked again, so a ledger that fell behind (the job
//! did not run, or a close failed) catches up one whole day at a time. If it
//! has not, nothing happens: a job that fires early or twice cannot close a
//! day that is still in progress.

use std::time::Duration;

use chrono::{DateTime, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Pacific::Auckland;
use ic_ledger::PartitionId;
use ic_raft::ClusterClient;

/// The instant a business date closes: the next midnight in New Zealand.
/// Daylight saving makes two days a year 23 or 25 hours long; the business
/// date does not care.
pub fn cutoff_utc(business_date: NaiveDate) -> Option<DateTime<Utc>> {
    let next = business_date.succ_opt()?.and_time(NaiveTime::MIN);
    match Auckland.from_local_datetime(&next) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        // Midnight never falls in New Zealand's daylight saving gap or
        // overlap (the changes are at 2am and 3am), but be exact anyway.
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

/// Whether a ledger on `business_date` is due to close at `now`.
pub fn is_due(business_date: NaiveDate, now: DateTime<Utc>) -> bool {
    cutoff_utc(business_date).is_some_and(|cutoff| now >= cutoff)
}

/// Where the ledger is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedgerDay {
    /// The earliest business date any partition is on: the day still to close.
    pub business_date: NaiveDate,
    pub partitions: PartitionId,
}

/// Ask the nodes how many partitions there are and what date they are on.
/// Part way through a close some partitions are a day ahead of others; the
/// earliest is the day the close has yet to finish.
pub async fn ledger_day(client: &ClusterClient, patience: Duration) -> Result<LedgerDay, String> {
    let partitions = client.partitions(patience).await?;
    let mut earliest: Option<NaiveDate> = None;
    for partition in 0..partitions {
        let deadline = tokio::time::Instant::now() + patience;
        let view = loop {
            match client.end_of_day_view(partition).await {
                Ok(view) => break view,
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.to_string()),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        };
        let date = view
            .business_date
            .ok_or_else(|| format!("partition {partition} has no business date: open the ledger with `ic-admin open`"))?;
        earliest = Some(earliest.map_or(date, |e| e.min(date)));
    }
    let business_date = earliest.ok_or("the ledger has no partitions")?;
    Ok(LedgerDay { business_date, partitions })
}
