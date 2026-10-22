//! Data model: Record keys. `main` has caller-assigned keys only; the
//! `Generated` mode, `.key(k)` and `KeyModeMismatch` do not compile against it yet.

use golemdb_conformance::*;

/// "`CallerAssigned` | must name the key | the caller's; an existing key is `AlreadyExists`"
#[test]
fn creating_a_committed_key_again_is_already_exists() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("a", CellValue::from_u32(1)),
    );
    let branch = db.begin().unwrap();
    assert_err!(
        db.create(branch, RecordOp::create(key(1), B)).result,
        ApiError::AlreadyExists
    );
    // Nothing was written: the original cell is intact.
    let record = read(&db, ReadTarget::Branch(branch), key(1));
    assert_eq!(user_cells(&record), cells([("a", CellValue::from_u32(1))]));
}

#[test]
fn creating_a_key_twice_in_one_branch_is_already_exists() {
    let db = db();
    let branch = db.begin().unwrap();
    db.create(branch, RecordOp::create(key(1), B)).ok();
    assert_err!(
        db.create(branch, RecordOp::create(key(1), B)).result,
        ApiError::AlreadyExists
    );
}

/// "never reused; a re-created key gets a new ID": a deleted key can be created again.
#[test]
fn a_deleted_key_can_be_created_again() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("old", CellValue::from_u32(1)),
    );
    in_commit(&db, |b| {
        db.delete(b, RecordOp::delete(key(1), B)).ok();
        db.create(
            b,
            RecordOp::create(key(1), B).field("new", CellValue::from_u32(2)),
        )
        .ok();
    });
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Head, key(1))),
        cells([("new", CellValue::from_u32(2))])
    );
}

/// Keys are 32 opaque bytes: keys differing in one byte are different records.
#[test]
fn keys_are_compared_on_all_32_bytes() {
    let db = db();
    let mut a = [7; 32];
    let mut b = [7; 32];
    a[31] = 0;
    b[31] = 1;
    in_commit(&db, |br| {
        db.create(
            br,
            RecordOp::create(RecordKey(a), B).field("v", CellValue::from_u32(0)),
        )
        .ok();
        db.create(
            br,
            RecordOp::create(RecordKey(b), B).field("v", CellValue::from_u32(1)),
        )
        .ok();
    });
    for (k, v) in [(a, 0), (b, 1)] {
        let record = read(&db, ReadTarget::Head, RecordKey(k));
        assert_eq!(user_cells(&record), cells([("v", CellValue::from_u32(v))]));
    }
}
