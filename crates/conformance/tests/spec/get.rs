//! Record operations: `get`.

use golemdb_conformance::*;

fn setup() -> TestDb {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", CellValue::from_u32(1))
            .field("b", CellValue::from_str("x")),
    );
    db
}

/// "A missing record is `NotFound`." — on every target.
#[test]
fn missing_record_is_not_found_on_every_target() {
    let db = setup();
    let branch = db.begin().unwrap();
    let head = db.head().unwrap();
    for target in [
        ReadTarget::Head,
        ReadTarget::Branch(branch),
        ReadTarget::Commit(head),
    ] {
        assert_err!(
            db.get(target, RecordOp::get(key(9), B)).result,
            ApiError::NotFound
        );
        assert_err!(
            db.get(target, RecordOp::get(key(9), B).only(["a"])).result,
            ApiError::NotFound
        );
    }
}

/// "`Head`: resolves the head and reads the record"; "`Commit(c)`: only the current head is supported."
#[test]
fn head_and_commit_of_head_read_the_same_record() {
    let db = setup();
    let head = db.head().unwrap();
    assert_eq!(
        read(&db, ReadTarget::Head, key(1)),
        read(&db, ReadTarget::Commit(head), key(1))
    );
}

/// "Any other commit is `CommitUnavailable { requested, head }`."
#[test]
fn other_commits_are_unavailable() {
    let db = setup();
    let head = db.head().unwrap();
    assert_eq!(head, CommitId::new(1));
    for requested in [CommitId::GENESIS, CommitId::new(2), CommitId::new(u64::MAX)] {
        assert_err!(
            db.get(ReadTarget::Commit(requested), RecordOp::get(key(1), B)).result,
            ApiError::CommitUnavailable { requested: r, head: h } if r == requested && h == head
        );
    }
}

/// "`Branch(id)`: work in progress, including every successful uncommitted mutation."
#[test]
fn branch_reads_see_uncommitted_writes_and_head_does_not() {
    let db = setup();
    let branch = db.begin().unwrap();
    db.patch(
        branch,
        RecordOp::patch(key(1), B).field("b", CellValue::from_str("y")),
    )
    .ok();
    assert_eq!(
        cell(&read(&db, ReadTarget::Branch(branch), key(1)), "b"),
        Some(&CellValue::from_str("y"))
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "b"),
        Some(&CellValue::from_str("x"))
    );
}

/// "A sealed branch stays readable and shows exactly the sealed state"
#[test]
fn sealed_branch_is_readable() {
    let db = setup();
    let branch = db.begin().unwrap();
    db.create(
        branch,
        RecordOp::create(key(2), B).field("c", CellValue::from_u32(3)),
    )
    .ok();
    db.seal(branch).unwrap();
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Branch(branch), key(2))),
        cells([("c", CellValue::from_u32(3))])
    );
}

/// "an invalid branch handle → `HandleInvalid`"
#[test]
fn unknown_or_consumed_branch_is_handle_invalid() {
    let db = setup();
    let committed = db.begin().unwrap();
    db.commit(committed).unwrap();
    let discarded = db.begin().unwrap();
    db.discard(discarded).unwrap();
    for branch in [committed, discarded, BranchId::new(u64::MAX)] {
        assert_err!(
            db.get(ReadTarget::Branch(branch), RecordOp::get(key(1), B))
                .result,
            ApiError::HandleInvalid
        );
    }
}

/// "With `.only(names)`, missing cells are omitted"
#[test]
fn projection_returns_only_named_cells_and_omits_missing_ones() {
    let db = setup();
    let record = db
        .get(
            ReadTarget::Head,
            RecordOp::get(key(1), B).only(["b", "nope", "#key"]),
        )
        .ok();
    let names: Vec<_> = record.cells.keys().map(|n| n.as_bytes().to_vec()).collect();
    assert_eq!(names, [b"#key".to_vec(), b"b".to_vec()]);
    assert_eq!(record.key, key(1));
}

/// "an empty list returns no cells but still checks that the record exists"
#[test]
fn empty_projection_is_an_existence_check() {
    let db = setup();
    let record = db
        .get(
            ReadTarget::Head,
            RecordOp::get(key(1), B).only(std::iter::empty::<&str>()),
        )
        .ok();
    assert!(record.cells.is_empty());
    assert_eq!(record.key, key(1));
    assert_err!(
        db.get(
            ReadTarget::Head,
            RecordOp::get(key(2), B).only(std::iter::empty::<&str>())
        )
        .result,
        ApiError::NotFound
    );
}

/// "`.only` takes names as raw bytes … duplicates collapse."
#[test]
fn projection_duplicates_collapse() {
    let db = setup();
    let record = db
        .get(
            ReadTarget::Head,
            RecordOp::get(key(1), B).only(["a", "a", "a"]),
        )
        .ok();
    assert_eq!(
        user_cells(&record),
        cells([("a", attr(CellValue::from_u32(1)))])
    );
}

/// A full read "returns every cell, `#key` and `#meta` included".
#[test]
fn full_read_includes_key_and_meta() {
    let db = setup();
    let record = read(&db, ReadTarget::Head, key(1));
    let system: Vec<_> = record
        .cells
        .keys()
        .filter(|n| n.as_bytes().starts_with(b"#"))
        .map(|n| n.as_bytes().to_vec())
        .collect();
    assert_eq!(system, [b"#key".to_vec(), b"#meta".to_vec()]);
}
