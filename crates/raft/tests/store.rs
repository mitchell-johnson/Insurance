//! A node's Raft log on disk, driven directly: what it holds after a
//! restart, after a new leader overwrites its tail, after a snapshot lets
//! it drop its head, and after a crash in the middle of a write.

mod common;

use std::io::Write;

use common::scratch;
use ic_core::{Command, Envelope};
use ic_raft::types::Housekeeping;
use ic_raft::{LogStore, Proposal, TypeConfig};
use openraft::alias::{EntryOf, VoteOf};
use openraft::entry::RaftEntry;
use openraft::storage::{IOFlushed, RaftLogReader, RaftLogStorage};
use openraft::testing::log_id;

fn command(key: &str) -> Proposal {
    Proposal::Command(Envelope {
        key: key.to_string(),
        caller: "leader".to_string(),
        salt: "salt".to_string(),
        traceparent: None,
        command: Command::OpenBusinessDate { date: common::start_date() },
    })
}

fn entry(term: u64, index: u64) -> EntryOf<TypeConfig> {
    EntryOf::<TypeConfig>::new_normal(log_id::<TypeConfig>(term, 1, index), command(&format!("t{term}-i{index}")))
}

/// What the log holds, as (term, index) pairs.
async fn held(store: &mut LogStore) -> Vec<(u64, u64)> {
    let entries = store.try_get_log_entries(..).await.unwrap();
    entries.iter().map(|e| (e.log_id().committed_leader_id().term, e.index())).collect()
}

/// Purpose: a node's log, vote and commit index are the same after a restart as before
/// it, through each thing Raft does to a log: appending, a new leader replacing an
/// uncommitted tail, a snapshot letting the head be dropped, and the whole log being
/// discarded.
/// Why it matters: Raft's safety rests on a node never forgetting what it acknowledged or
/// whom it voted for. A node that restarted with entries its leader had overwritten, or
/// without its vote, could vote twice in one term or resurrect a command that was never
/// committed: two different ledgers on two nodes.
#[tokio::test]
async fn the_raft_log_is_the_same_after_a_restart() {
    let dir = scratch("store-restart");
    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(store.get_log_state().await.unwrap().last_log_id, None);
    store.append((1..=5).map(|i| entry(1, i)), IOFlushed::noop()).await.unwrap();
    let vote = VoteOf::<TypeConfig>::new(2, 3);
    store.save_vote(&vote).await.unwrap();
    store.save_committed(Some(log_id::<TypeConfig>(1, 1, 3))).await.unwrap();
    assert_eq!(store.commands().len(), 3, "only committed commands are an auditor's to replay");

    // A new leader in term 2 never had entries 4 and 5: they are replaced.
    store.truncate_after(Some(log_id::<TypeConfig>(1, 1, 3))).await.unwrap();
    store.append([entry(2, 4), entry(2, 5), entry(2, 6)], IOFlushed::noop()).await.unwrap();
    let expected = vec![(1, 1), (1, 2), (1, 3), (2, 4), (2, 5), (2, 6)];
    assert_eq!(held(&mut store).await, expected);
    drop(store);

    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(held(&mut store).await, expected, "the overwritten tail came back");
    assert_eq!(store.read_vote().await.unwrap(), Some(vote));
    assert_eq!(store.read_committed().await.unwrap(), Some(log_id::<TypeConfig>(1, 1, 3)));
    assert_eq!((store.first_index(), store.last_index()), (Some(1), Some(6)));

    // A snapshot through entry 4 lets the head go.
    let through = log_id::<TypeConfig>(2, 1, 4);
    store.purge(through).await.unwrap();
    store.append([entry(2, 7)], IOFlushed::noop()).await.unwrap();
    drop(store);
    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(held(&mut store).await, vec![(2, 5), (2, 6), (2, 7)]);
    let state = store.get_log_state().await.unwrap();
    assert_eq!(state.last_purged_log_id, Some(through));
    assert_eq!(state.last_log_id, Some(log_id::<TypeConfig>(2, 1, 7)));
    let mut reader = store.get_log_reader().await;
    assert_eq!(reader.try_get_log_entries(6..=6).await.unwrap().len(), 1);

    // Everything discarded.
    store.truncate_after(None).await.unwrap();
    drop(store);
    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(held(&mut store).await, vec![]);
    assert_eq!(
        store.get_log_state().await.unwrap().last_purged_log_id,
        Some(through),
        "what was purged stays purged"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: a log whose last record was cut short by a crash opens without that record
/// and goes on appending cleanly, while a log, vote or commit file that is damaged
/// anywhere else refuses to open at all.
/// Why it matters: a half-written last line was never acknowledged to anyone, so dropping
/// it is safe and is what lets a node restart after a power cut. Damage elsewhere is
/// different: those records were acknowledged, and a node that opened anyway and guessed
/// would rejoin its group with a log that disagrees with what it promised.
#[tokio::test]
async fn a_torn_tail_is_dropped_and_other_damage_is_refused() {
    let dir = scratch("store-damage");
    let mut store = LogStore::open(&dir).unwrap();
    store.append([entry(1, 1), entry(1, 2)], IOFlushed::noop()).await.unwrap();
    store.save_vote(&VoteOf::<TypeConfig>::new(1, 1)).await.unwrap();
    drop(store);
    let journal = dir.join("log.jsonl");
    let intact = std::fs::read(&journal).unwrap();

    // The crash: half a record, no newline.
    let mut file = std::fs::OpenOptions::new().append(true).open(&journal).unwrap();
    file.write_all(br#"{"append":{"log_id":{"leader"#).unwrap();
    drop(file);
    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(held(&mut store).await, vec![(1, 1), (1, 2)]);
    assert_eq!(std::fs::read(&journal).unwrap(), intact, "the torn tail was not cut off");
    store.append([entry(1, 3)], IOFlushed::noop()).await.unwrap();
    drop(store);
    let mut store = LogStore::open(&dir).unwrap();
    assert_eq!(held(&mut store).await, vec![(1, 1), (1, 2), (1, 3)]);
    drop(store);

    // A complete line that is not a record.
    let good = std::fs::read(&journal).unwrap();
    std::fs::write(&journal, [b"not a record\n".as_slice(), good.as_slice()].concat()).unwrap();
    let refused = LogStore::open(&dir).err().expect("a damaged journal was opened");
    assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
    std::fs::write(&journal, &good).unwrap();

    // A vote file that cannot be read is not the same as no vote.
    std::fs::write(dir.join("vote.json"), "{").unwrap();
    assert_eq!(LogStore::open(&dir).err().map(|e| e.kind()), Some(std::io::ErrorKind::InvalidData));
    std::fs::remove_file(dir.join("vote.json")).unwrap();
    std::fs::create_dir(dir.join("vote.json")).unwrap();
    assert!(LogStore::open(&dir).is_err(), "an unreadable vote was treated as no vote");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Purpose: a log entry describes itself in a log line by what it is, who sent it and its
/// idempotency key, and never by its payload.
/// Why it matters: openraft writes these descriptions into the node's logs, which go to
/// Azure Monitor. A command's payload carries names, dates of birth and health
/// disclosures; printing it there would copy personal data into a system with a different
/// access policy and retention from the ledger's.
#[test]
fn log_lines_name_a_command_without_its_payload() {
    let line = command("open-2027-11-15").to_string();
    assert_eq!(line, "open_business_date from leader (open-2027-11-15)");
    assert!(!line.contains("salt"));
    let delivered = Proposal::Housekeeping(Housekeeping::Delivered {
        ids: vec!["a".to_string(), "b".to_string()],
    });
    assert_eq!(delivered.to_string(), "delivered 2 messages");
    assert_eq!(
        Proposal::Housekeeping(Housekeeping::Sealed { through_seq: 40 }).to_string(),
        "sealed journal through 40"
    );
    assert_eq!(
        Proposal::Housekeeping(Housekeeping::Published { through_seq: 7 }).to_string(),
        "published events through 7"
    );
    let held = Housekeeping::Consumed {
        consumer: "finance".to_string(),
        through_seq: 9,
    };
    assert_eq!(Proposal::Housekeeping(held).to_string(), "finance holds events through 9");
    let gone = Housekeeping::Unsubscribed {
        consumer: "finance".to_string(),
    };
    assert_eq!(Proposal::Housekeeping(gone).to_string(), "finance no longer reads events");
    assert_eq!(Proposal::Module(serde_json::json!({ "name": "A Person" })).to_string(), "a module command");
}
