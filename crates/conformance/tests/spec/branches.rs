//! Branches.

use std::sync::{Arc, Barrier};

use golemdb_conformance::*;

fn x(db: &Database, b: BranchId) -> Option<u32> {
    match db
        .get(ReadTarget::Branch(b), RecordOp::get(key(1), B).only(["x"]))
        .result
    {
        Ok(record) => cell(&record, "x").map(|v| v.as_u32().unwrap()),
        Err(ApiError::NotFound) => None,
        Err(e) => panic!("{e:?}"),
    }
}

fn set_x(db: &Database, b: BranchId, v: u32) {
    db.patch(
        b,
        RecordOp::patch(key(1), B).field("x", CellValue::from_u32(v)),
    )
    .ok();
}

fn setup() -> (Database, BranchId) {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("x", CellValue::from_u32(0)),
    );
    let branch = db.begin().unwrap();
    (db, branch)
}

/// "`CommitId` … 0 = genesis (`CommitId::GENESIS`), +1 per commit"
#[test]
fn head_starts_at_genesis_and_advances_by_one_per_commit() {
    let db = db();
    assert_eq!(db.head().unwrap(), CommitId::GENESIS);
    for n in 1..=3 {
        let commit = in_commit(&db, |_| {});
        assert_eq!(commit, CommitId::new(n));
        assert_eq!(db.head().unwrap(), commit);
    }
}

/// "`begin()` | a new branch over the current head"; "`branch_info(b)` |
/// `commit_id` = origin; … `sealed`"
#[test]
fn branch_info_reports_origin_and_state() {
    let (db, b) = setup();
    let info = db.branch_info(b).unwrap();
    assert_eq!(info.commit_id, CommitId::new(1));
    assert_eq!(info.branch_id, b);
    assert!(!info.sealed);
    db.seal(b).unwrap();
    assert!(db.branch_info(b).unwrap().sealed);
}

/// "`BranchId(u64)`: process-local, monotonic, never reused"
#[test]
fn branch_ids_are_monotonic_and_never_reused() {
    let db = db();
    let mut previous = db.begin().unwrap();
    for i in 0..10 {
        if i % 2 == 0 {
            db.discard(previous).unwrap();
        }
        let next = db.begin().unwrap();
        assert!(next > previous, "{next} after {previous}");
        previous = next;
    }
}

/// "`version` = number of retained undo entries, not a revision counter
/// (rollback lowers it, an identical write adds none)"
#[test]
fn version_counts_retained_undo_entries() {
    let (db, b) = setup();
    let start = db.branch_info(b).unwrap().version;
    set_x(&db, b, 1);
    let after_write = db.branch_info(b).unwrap().version;
    assert!(after_write > start);
    set_x(&db, b, 1);
    assert_eq!(db.branch_info(b).unwrap().version, after_write);
    db.rollback(b).unwrap();
    assert_eq!(db.branch_info(b).unwrap().version, start);
}

/// "`rollback(b)` | restores the start of the current frame"; `begin` opens one frame.
#[test]
fn rollback_without_checkpoint_undoes_everything_since_begin() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.create(b, RecordOp::create(key(2), B)).ok();
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(0));
    assert_err!(
        db.get(ReadTarget::Branch(b), RecordOp::get(key(2), B))
            .result,
        ApiError::NotFound
    );
}

/// "`checkpoint(b)` | ends the current frame and opens a new one"
#[test]
fn rollback_after_checkpoint_undoes_only_the_current_frame() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.checkpoint(b).unwrap();
    set_x(&db, b, 2);
    db.delete(b, RecordOp::delete(key(1), B)).ok();
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(1));
}

/// "A rollback restores the current frame's starting state and keeps its marker.
/// Another rollback without an intervening write or checkpoint steps back to the preceding frame."
#[test]
fn a_second_rollback_steps_back_a_frame() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.checkpoint(b).unwrap();
    set_x(&db, b, 2);
    db.checkpoint(b).unwrap();
    set_x(&db, b, 3);
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(2));
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(1));
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(0));
}

/// "Fresh empty frames are not skipped."
#[test]
fn empty_frames_are_not_skipped() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.checkpoint(b).unwrap();
    db.checkpoint(b).unwrap();
    db.rollback(b).unwrap(); // the empty last frame
    assert_eq!(x(&db, b), Some(1));
    db.rollback(b).unwrap(); // the empty frame before it
    assert_eq!(x(&db, b), Some(1));
    db.rollback(b).unwrap(); // the first frame
    assert_eq!(x(&db, b), Some(0));
}

/// "Once the first frame has been rolled back, a further rollback is
/// `NoFrameToRollback`; a write or checkpoint makes the last marker usable again."
#[test]
fn rolling_back_past_the_first_frame_is_no_frame_to_rollback() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.rollback(b).unwrap();
    assert_err!(db.rollback(b), ApiError::NoFrameToRollback);
    assert_err!(db.rollback(b), ApiError::NoFrameToRollback);
    // A write makes the marker usable again.
    set_x(&db, b, 2);
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(0));
    assert_err!(db.rollback(b), ApiError::NoFrameToRollback);
    // So does a checkpoint.
    db.checkpoint(b).unwrap();
    db.rollback(b).unwrap();
    assert_eq!(x(&db, b), Some(0));
}

/// Rollback on a fresh branch restores the (empty) first frame once.
#[test]
fn rollback_on_a_fresh_branch() {
    let (db, b) = setup();
    db.rollback(b).unwrap();
    assert_err!(db.rollback(b), ApiError::NoFrameToRollback);
    assert_eq!(x(&db, b), Some(0));
}

/// A rolled-back branch commits only what is left.
#[test]
fn commit_after_rollback_persists_only_the_kept_frames() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.checkpoint(b).unwrap();
    db.create(b, RecordOp::create(key(2), B)).ok();
    db.rollback(b).unwrap();
    db.commit(b).unwrap();
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(1))
    );
    assert_err!(
        db.get(ReadTarget::Head, RecordOp::get(key(2), B)).result,
        ApiError::NotFound
    );
}

/// "`seal(b)` | computes both roots and freezes the branch; publishes nothing.
/// The `commit_id` it returns is provisional"
#[test]
fn seal_publishes_nothing() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    let sealed = db.seal(b).unwrap();
    assert_eq!(sealed.commit_id, CommitId::new(2));
    assert_eq!(db.head().unwrap(), CommitId::new(1));
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(0))
    );
}

/// "A repeated seal returns the cached result"
#[test]
fn repeated_seal_returns_the_same_result() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    assert_eq!(db.seal(b).unwrap(), db.seal(b).unwrap());
}

/// "A sealed branch rejects record writes, `checkpoint` and `rollback` with
/// `Sealed`… `get`, `branch_info`, `commit` and `discard` remain available."
#[test]
fn sealed_branch_rejects_writes_checkpoint_and_rollback() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.seal(b).unwrap();
    assert_err!(
        db.create(b, RecordOp::create(key(2), B)).result,
        ApiError::Sealed
    );
    assert_err!(
        db.patch(
            b,
            RecordOp::patch(key(1), B).field("x", CellValue::from_u32(2))
        )
        .result,
        ApiError::Sealed
    );
    assert_err!(
        db.delete(b, RecordOp::delete(key(1), B)).result,
        ApiError::Sealed
    );
    assert_err!(db.checkpoint(b), ApiError::Sealed);
    assert_err!(db.rollback(b), ApiError::Sealed);
    // "the handle stays valid"
    assert_eq!(x(&db, b), Some(1));
    db.branch_info(b).unwrap();
    db.commit(b).unwrap();
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(1))
    );
}

#[test]
fn a_sealed_branch_can_be_discarded() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.seal(b).unwrap();
    db.discard(b).unwrap();
    assert_eq!(db.head().unwrap(), CommitId::new(1));
    assert_err!(db.branch_info(b), ApiError::HandleInvalid);
}

/// "`commit(b)` | seals if needed, then persists atomically and advances the head;
/// consumes the branch". The committed roots are the sealed ones.
#[test]
fn commit_returns_the_sealed_commit_and_consumes_the_branch() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    let sealed = db.seal(b).unwrap();
    assert_eq!(db.commit(b).unwrap(), sealed.commit_id);
    assert_eq!(db.head().unwrap(), sealed.commit_id);
    assert_err!(db.branch_info(b), ApiError::HandleInvalid);
    assert_err!(db.commit(b), ApiError::HandleInvalid);
    // The next commit records them in #roots (lag-one).
    in_commit(&db, |_| {});
    let roots = read(&db, ReadTarget::Head, reserved_key("#roots"));
    let expected = [sealed.state_root, sealed.index_root].concat();
    assert_eq!(
        roots
            .cells
            .get(sealed.commit_id.get().to_be_bytes().as_slice()),
        Some(&CellValue::from_bytes(&expected))
    );
}

/// Commit without an explicit seal gives the same roots as seal then commit.
#[test]
fn commit_seals_if_needed() {
    let roots = |seal_first: bool| {
        let dir = tempfile::tempdir().unwrap();
        let config = OpenConfig::new(genesis());
        {
            let db = Database::open(dir.path(), &config).unwrap();
            let b = db.begin().unwrap();
            db.create(
                b,
                RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
            )
            .ok();
            if seal_first {
                db.seal(b).unwrap();
            }
            db.commit(b).unwrap();
        }
        let info = *Database::open(dir.path(), &config).unwrap().info();
        (info.commit_id, info.state_root, info.index_root)
    };
    assert_eq!(roots(true), roots(false));
}

/// "`discard(b)` | drops the branch without replay; consumes it"
#[test]
fn discard_drops_the_work_and_consumes_the_branch() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.discard(b).unwrap();
    assert_err!(db.branch_info(b), ApiError::HandleInvalid);
    assert_err!(db.discard(b), ApiError::HandleInvalid);
    assert_eq!(db.head().unwrap(), CommitId::new(1));
    let fresh = db.begin().unwrap();
    assert_eq!(x(&db, fresh), Some(0));
}

/// An empty branch commits: a new commit with no record changes.
#[test]
fn an_empty_branch_commits() {
    let (db, b) = setup();
    assert_eq!(db.commit(b).unwrap(), CommitId::new(2));
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(0))
    );
}

/// A branch begun after a commit sees it.
#[test]
fn a_new_branch_starts_from_the_head() {
    let (db, b) = setup();
    set_x(&db, b, 1);
    db.commit(b).unwrap();
    let next = db.begin().unwrap();
    assert_eq!(db.branch_info(next).unwrap().commit_id, CommitId::new(2));
    assert_eq!(x(&db, next), Some(1));
}

/// "Branches over the same head are independent until one commits."
#[test]
fn concurrent_branches_do_not_see_each_other() {
    let (db, a) = setup();
    let b = db.begin().unwrap();
    set_x(&db, a, 1);
    set_x(&db, b, 2);
    assert_eq!(x(&db, a), Some(1));
    assert_eq!(x(&db, b), Some(2));
}

/// "On success the branch is consumed, and every other branch over the old head
/// becomes stale." "Every call on it, `commit` included, returns `HandleInvalid`"
#[test]
fn every_call_on_a_stale_branch_is_handle_invalid() {
    type Call = fn(&Database, BranchId) -> Result<()>;
    // `commit` is checked on its own below.
    let calls: [(&str, Call); 9] = [
        ("branch_info", |db, b| db.branch_info(b).map(drop)),
        ("checkpoint", |db, b| db.checkpoint(b)),
        ("rollback", |db, b| db.rollback(b)),
        ("seal", |db, b| db.seal(b).map(drop)),
        ("discard", |db, b| db.discard(b)),
        ("get", |db, b| {
            db.get(ReadTarget::Branch(b), RecordOp::get(key(1), B))
                .result
                .map(drop)
        }),
        ("create", |db, b| {
            db.create(b, RecordOp::create(key(2), B)).result.map(drop)
        }),
        ("patch", |db, b| {
            db.patch(b, RecordOp::patch(key(1), B)).result
        }),
        ("delete", |db, b| {
            db.delete(b, RecordOp::delete(key(1), B)).result
        }),
    ];
    for (name, call) in calls {
        for sealed in [false, true] {
            let (db, stale) = setup();
            set_x(&db, stale, 1);
            if sealed {
                db.seal(stale).unwrap();
            }
            let winner = db.begin().unwrap();
            db.commit(winner).unwrap();
            let result = call(&db, stale);
            assert!(
                matches!(result, Err(ApiError::HandleInvalid)),
                "{name} (sealed: {sealed}): {result:?}"
            );
            // "the first call that detects it removes the branch"
            assert_err!(db.branch_info(stale), ApiError::HandleInvalid);
        }
    }
}

/// "Every call on it, `commit` included, returns `HandleInvalid`"
#[test]
#[ignore = "spec-gap: a stale commit returns Conflict, not HandleInvalid (Agreed changes, D15)"]
fn commit_on_a_stale_branch_is_handle_invalid() {
    for sealed in [false, true] {
        let (db, stale) = setup();
        set_x(&db, stale, 1);
        if sealed {
            db.seal(stale).unwrap();
        }
        let winner = db.begin().unwrap();
        db.commit(winner).unwrap();
        assert_err!(db.commit(stale), ApiError::HandleInvalid);
    }
}

/// The losing branch's work is not visible anywhere.
#[test]
fn a_stale_branchs_writes_are_lost() {
    let (db, loser) = setup();
    let winner = db.begin().unwrap();
    set_x(&db, loser, 1);
    set_x(&db, winner, 2);
    db.commit(winner).unwrap();
    assert!(db.commit(loser).is_err());
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(2))
    );
    assert_err!(db.branch_info(loser), ApiError::HandleInvalid);
}

/// "Clones share state. All clones of a `Database` share one branch registry, so
/// a branch begun through one clone works through any other and across threads."
#[test]
fn clones_share_branches_across_threads() {
    let (db, b) = setup();
    let clone = db.clone();
    std::thread::spawn(move || set_x(&clone, b, 7))
        .join()
        .unwrap();
    assert_eq!(x(&db, b), Some(7));
    let clone = db.clone();
    std::thread::spawn(move || clone.commit(b).unwrap())
        .join()
        .unwrap();
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(7))
    );
}

/// Many branches over one head, sealed, then committed at once from threads.
fn race(writers: u32) -> (Database, Vec<Result<u32>>) {
    let (db, _) = setup();
    let barrier = Arc::new(Barrier::new(writers as usize));
    let threads: Vec<_> = (0..writers)
        .map(|i| {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let b = db.begin().unwrap();
                set_x(&db, b, 100 + i);
                db.seal(b).unwrap();
                barrier.wait();
                db.commit(b).map(|_| 100 + i)
            })
        })
        .collect();
    let results = threads.into_iter().map(|t| t.join().unwrap()).collect();
    (db, results)
}

/// "The head is checked again inside [the store writer] before any row is
/// written": of many branches racing over one head, exactly one commits.
#[test]
fn exactly_one_of_racing_commits_wins() {
    let (db, results) = race(8);
    let winners: Vec<u32> = results
        .iter()
        .filter_map(|r| r.as_ref().ok().copied())
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert_eq!(db.head().unwrap(), CommitId::new(2));
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_u32(winners[0]))
    );
}

/// "This includes a `commit` that loses the race inside the store writer, after
/// its handle was validated."
#[test]
#[ignore = "spec-gap: a stale commit returns Conflict, not HandleInvalid (Agreed changes, D15)"]
fn losers_of_a_commit_race_get_handle_invalid() {
    let (_, results) = race(8);
    for result in results.iter().filter(|r| r.is_err()) {
        assert!(matches!(result, Err(ApiError::HandleInvalid)), "{result:?}");
    }
}
