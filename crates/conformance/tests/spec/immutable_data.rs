//! Immutable data. The API spec's section still describes stubs; the contract
//! here is its "Intended contract" plus the design doc, §11 "Appending to a
//! Branch", "Appends are staged, not streamed" and "Row keys". `range_of` is
//! not on `main` and does not compile against it yet.

use golemdb_conformance::*;

/// Two segments of opaque byte columns; a shard spans two commits, so a few
/// commits cross shard files.
fn immutable_genesis() -> Genesis {
    let segment = |name: &str, columns: &[&str]| ImmutableDataSegment {
        name: name.into(),
        columns: columns
            .iter()
            .map(|c| ImmutableDataColumn::new(*c))
            .collect(),
        compression: ImmutableDataCompression::None,
    };
    Genesis::new(
        HashAlgorithm::Keccak256,
        LIMITS,
        RECORD_LIMITS,
        Some(ImmutableDataConfig::new(
            2,
            vec![segment("headers", &["h"]), segment("bodies", &["a", "b"])],
        )),
    )
}

fn idb() -> TestDb {
    open_db(&immutable_genesis())
}

fn header(n: u8) -> ImmutableDataRow {
    ImmutableDataRow::new(vec![CellValue::from_bytes(&[n; 3])])
}

fn body(n: u8) -> ImmutableDataRow {
    ImmutableDataRow::new(vec![
        CellValue::from_bytes(&[n]),
        CellValue::from_bytes(&vec![n; 1000]),
    ])
}

fn rkey(n: u8) -> ImmutableDataKey {
    ImmutableDataKey([n; 32])
}

fn all() -> ImmutableDataReadOptions {
    ImmutableDataReadOptions::all()
}

fn append(db: &Database, b: BranchId, n: u8) -> ImmutableDataOrdinal {
    db.immutable_data_append(b, "headers", None, header(n))
        .unwrap()
}

fn at(db: &Database, ordinal: u64) -> Result<ImmutableDataRow> {
    db.immutable_data_get("headers", ImmutableDataAddress::Ordinal(ordinal), &all())
}

fn by_key(db: &Database, segment: &str, n: u8) -> Result<ImmutableDataRow> {
    db.immutable_data_get(segment, ImmutableDataAddress::Key(rkey(n)), &all())
}

fn rows_of(db: &Database, commit: CommitId) -> Vec<ImmutableDataRow> {
    db.immutable_data_rows_of("headers", commit, &all())
        .unwrap()
}

/// "ordinal | A dense `u64`, monotonic per segment, assigned in append order";
/// "ordinals are dense within one segment and mean nothing across two".
#[test]
fn ordinals_are_dense_per_segment_and_continue_across_commits() {
    let db = idb();
    in_commit(&db, |b| {
        assert_eq!(append(&db, b, 0), 0);
        assert_eq!(append(&db, b, 1), 1);
        assert_eq!(
            db.immutable_data_append(b, "bodies", None, body(0))
                .unwrap(),
            0
        );
        assert_eq!(append(&db, b, 2), 2);
    });
    in_commit(&db, |b| {
        assert_eq!(append(&db, b, 3), 3);
        assert_eq!(
            db.immutable_data_append(b, "bodies", None, body(1))
                .unwrap(),
            1
        );
    });
    for n in 0..4u8 {
        assert_eq!(at(&db, n as u64).unwrap(), header(n));
    }
}

/// "`immutable_data_rows_of` | `(seg, commitNr) → [row]` | the whole run";
/// "returns that commit's rows in append order". A commit appending nothing has none.
#[test]
fn rows_of_returns_each_commits_run_in_append_order() {
    let db = idb();
    let c1 = in_commit(&db, |b| {
        for n in [5, 1, 9] {
            append(&db, b, n);
        }
    });
    let c2 = in_commit(&db, |_| {});
    let c3 = in_commit(&db, |b| {
        append(&db, b, 7);
    });
    assert_eq!(rows_of(&db, CommitId::GENESIS), vec![]);
    assert_eq!(rows_of(&db, c1), vec![header(5), header(1), header(9)]);
    assert_eq!(rows_of(&db, c2), vec![]);
    assert_eq!(rows_of(&db, c3), vec![header(7)]);
    assert_eq!(at(&db, 3).unwrap(), header(7));
}

/// "Staged rows are not readable until `commit`"
#[test]
fn staged_rows_are_not_readable_before_commit() {
    let db = idb();
    let b = db.begin().unwrap();
    let ordinal = db
        .immutable_data_append(b, "headers", Some(rkey(1)), header(1))
        .unwrap();
    assert_err!(at(&db, ordinal), ApiError::NotFound);
    assert_err!(by_key(&db, "headers", 1), ApiError::NotFound);
    db.seal(b).unwrap();
    assert_err!(at(&db, ordinal), ApiError::NotFound);
    let commit = db.commit(b).unwrap();
    assert_eq!(at(&db, ordinal).unwrap(), header(1));
    assert_eq!(by_key(&db, "headers", 1).unwrap(), header(1));
    assert_eq!(rows_of(&db, commit), vec![header(1)]);
}

/// "After `seal`, only appends are allowed … Rows are outside the commitment,
/// so appending them never changes the sealed roots."
#[test]
fn appends_after_seal_are_allowed_and_do_not_change_the_roots() {
    let db = idb();
    let b = db.begin().unwrap();
    db.create(b, RecordOp::create(key(1), B)).ok();
    append(&db, b, 1);
    let sealed = db.seal(b).unwrap();
    // A row may now carry its own commit's root.
    let row = ImmutableDataRow::new(vec![CellValue::from_bytes(&sealed.state_root)]);
    assert_eq!(
        db.immutable_data_append(b, "headers", None, row.clone())
            .unwrap(),
        1
    );
    assert_eq!(db.seal(b).unwrap(), sealed);
    db.commit(b).unwrap();
    assert_eq!(at(&db, 1).unwrap(), row);
    // The committed roots are the sealed ones: the next commit records them.
    in_commit(&db, |_| {});
    let roots = read(&db, ReadTarget::Head, reserved_key("#roots"));
    assert_eq!(
        roots.cells.get(1u64.to_be_bytes().as_slice()),
        Some(&CellValue::from_bytes(
            &[sealed.state_root, sealed.index_root].concat()
        ))
    );
}

/// Rows are outside the commitment: a branch with rows has the same roots as one without.
#[test]
fn rows_never_enter_the_state_commitment() {
    let sealed = |with_rows: bool| {
        let db = idb();
        let b = db.begin().unwrap();
        db.create(
            b,
            RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
        )
        .ok();
        if with_rows {
            append(&db, b, 1);
            db.immutable_data_append(b, "bodies", Some(rkey(1)), body(1))
                .unwrap();
        }
        db.seal(b).unwrap()
    };
    assert_eq!(sealed(true), sealed(false));
}

/// "Before `seal`, an append belongs to the current frame like any write.
/// `rollback()` drops the rows the undone frame staged"
#[test]
#[ignore = "spec-gap: rollback keeps staged rows (main drops them with immutable_data_truncate, which the design keeps out of the API)"]
fn rollback_drops_the_rows_of_the_undone_frame() {
    let db = idb();
    let b = db.begin().unwrap();
    append(&db, b, 0);
    db.checkpoint(b).unwrap();
    append(&db, b, 1);
    db.immutable_data_append(b, "headers", Some(rkey(2)), header(2))
        .unwrap();
    db.rollback(b).unwrap();
    // The undone ordinals are handed out again.
    assert_eq!(append(&db, b, 3), 1);
    let commit = db.commit(b).unwrap();
    assert_eq!(rows_of(&db, commit), vec![header(0), header(3)]);
    assert_err!(by_key(&db, "headers", 2), ApiError::NotFound);
}

/// Rolling back the first frame drops every staged row.
#[test]
#[ignore = "spec-gap: rollback keeps staged rows (main drops them with immutable_data_truncate, which the design keeps out of the API)"]
fn rollback_of_the_first_frame_drops_all_rows() {
    let db = idb();
    let b = db.begin().unwrap();
    append(&db, b, 0);
    append(&db, b, 1);
    db.rollback(b).unwrap();
    let commit = db.commit(b).unwrap();
    assert_eq!(rows_of(&db, commit), vec![]);
    assert_err!(at(&db, 0), ApiError::NotFound);
}

/// "The ordinal `immutable_data_append` returns is provisional — `mark + i`, valid
/// only if this branch commits." "A losing branch leaves no trace in the segment file."
#[test]
fn a_losing_branch_leaves_no_trace() {
    let db = idb();
    let winner = db.begin().unwrap();
    let loser = db.begin().unwrap();
    assert_eq!(
        db.immutable_data_append(loser, "headers", Some(rkey(9)), header(9))
            .unwrap(),
        0
    );
    assert_eq!(
        db.immutable_data_append(loser, "headers", None, header(8))
            .unwrap(),
        1
    );
    assert_eq!(append(&db, winner, 1), 0);
    db.seal(loser).unwrap();
    let commit = db.commit(winner).unwrap();
    assert!(db.commit(loser).is_err());
    assert_eq!(rows_of(&db, commit), vec![header(1)]);
    assert_eq!(at(&db, 0).unwrap(), header(1));
    assert_err!(at(&db, 1), ApiError::NotFound);
    assert_err!(by_key(&db, "headers", 9), ApiError::NotFound);
    // The next branch continues from the winner's mark.
    let next = db.begin().unwrap();
    assert_eq!(append(&db, next, 2), 1);
}

/// "Uniqueness is the host's job. … An append whose key already exists points the
/// index at the new row: **the newest row wins**. Older rows … stay readable by
/// ordinal … An append never fails because of its key"
#[test]
#[ignore = "spec-gap: a repeated row key is AlreadyExists instead of pointing the index at the newest row"]
fn a_repeated_key_points_at_the_newest_row() {
    let db = idb();
    in_commit(&db, |b| {
        db.immutable_data_append(b, "headers", Some(rkey(1)), header(10))
            .unwrap();
    });
    in_commit(&db, |b| {
        db.immutable_data_append(b, "headers", Some(rkey(1)), header(11))
            .unwrap();
        db.immutable_data_append(b, "headers", Some(rkey(1)), header(12))
            .unwrap();
    });
    assert_eq!(by_key(&db, "headers", 1).unwrap(), header(12));
    assert_eq!(at(&db, 0).unwrap(), header(10));
    assert_eq!(at(&db, 1).unwrap(), header(11));
}

/// "Per segment. The same key may appear in several segments"
#[test]
fn keys_are_per_segment() {
    let db = idb();
    in_commit(&db, |b| {
        db.immutable_data_append(b, "headers", Some(rkey(1)), header(1))
            .unwrap();
        db.immutable_data_append(b, "bodies", Some(rkey(1)), body(1))
            .unwrap();
        db.immutable_data_append(b, "headers", Some(rkey(2)), header(2))
            .unwrap();
    });
    assert_eq!(by_key(&db, "headers", 1).unwrap(), header(1));
    assert_eq!(by_key(&db, "bodies", 1).unwrap(), body(1));
    assert_eq!(by_key(&db, "headers", 2).unwrap(), header(2));
    assert_err!(by_key(&db, "bodies", 2), ApiError::NotFound);
}

/// A row is addressed by `(segment, ordinal)` or its key; anything else is `NotFound`.
#[test]
fn missing_rows_are_not_found() {
    let db = idb();
    in_commit(&db, |b| {
        append(&db, b, 0);
    });
    assert_err!(at(&db, 1), ApiError::NotFound);
    assert_err!(at(&db, u64::MAX), ApiError::NotFound);
    assert_err!(by_key(&db, "headers", 7), ApiError::NotFound);
}

/// "A row … one value per column"; "its column arity … fixed at genesis".
#[test]
fn rows_of_the_wrong_arity_are_rejected() {
    let db = idb();
    let b = db.begin().unwrap();
    for row in [
        ImmutableDataRow::new(vec![]),
        ImmutableDataRow::new(vec![
            CellValue::from_bytes(b"a"),
            CellValue::from_bytes(b"b"),
        ]),
    ] {
        assert_err!(
            db.immutable_data_append(b, "headers", None, row),
            ApiError::InvalidArgument { .. }
        );
    }
    assert_eq!(append(&db, b, 0), 0);
}

/// "Columns exist so that a segment can be … read position by position": a
/// projection returns the named columns.
#[test]
fn reads_can_select_columns() {
    let db = idb();
    in_commit(&db, |b| {
        db.immutable_data_append(b, "bodies", None, body(4))
            .unwrap();
    });
    let row = db
        .immutable_data_get(
            "bodies",
            ImmutableDataAddress::Ordinal(0),
            &ImmutableDataReadOptions::columns(["b"]),
        )
        .unwrap();
    assert_eq!(row.values(), &[CellValue::from_bytes(&vec![4; 1000])]);
}

/// The segments declared at genesis are the only ones.
#[test]
fn unknown_segments_are_rejected() {
    let db = idb();
    let b = db.begin().unwrap();
    assert!(
        db.immutable_data_append(b, "nope", None, header(0))
            .is_err()
    );
    assert!(
        db.immutable_data_get("nope", ImmutableDataAddress::Ordinal(0), &all())
            .is_err()
    );
    assert!(
        db.immutable_data_rows_of("nope", CommitId::GENESIS, &all())
            .is_err()
    );
    let plain = golemdb_conformance::db(); // no immutable data declared
    let b = plain.begin().unwrap();
    assert!(
        plain
            .immutable_data_append(b, "headers", None, header(0))
            .is_err()
    );
}

/// "Deterministic: the same operations in the same order yield the same ordinals",
/// and committed rows survive a reopen.
#[test]
fn rows_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let config = OpenConfig::new(immutable_genesis());
    let mut commits = Vec::new();
    {
        let db = Database::open(dir.path(), &config).unwrap();
        for c in 0..5u8 {
            commits.push(in_commit(&db, |b| {
                for i in 0..c {
                    let n = c * 10 + i;
                    db.immutable_data_append(b, "headers", Some(rkey(n)), header(n))
                        .unwrap();
                }
            }));
        }
    }
    let db = Database::open(dir.path(), &config).unwrap();
    let mut ordinal = 0;
    for (c, commit) in commits.iter().enumerate() {
        let c = c as u8;
        let expected: Vec<_> = (0..c).map(|i| header(c * 10 + i)).collect();
        assert_eq!(rows_of(&db, *commit), expected);
        for i in 0..c {
            assert_eq!(at(&db, ordinal).unwrap(), header(c * 10 + i));
            assert_eq!(
                by_key(&db, "headers", c * 10 + i).unwrap(),
                header(c * 10 + i)
            );
            ordinal += 1;
        }
    }
    // The next append continues the dense sequence.
    let b = db.begin().unwrap();
    assert_eq!(append(&db, b, 99), ordinal);
}
