//! Record operations: `delete`.

use golemdb_conformance::*;

fn setup() -> TestDb {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", CellValue::from_u32(1))
            .field("f", CellValue::from_str("x")),
    );
    db
}

/// "It then removes every live cell of the record, `#key` and `#meta` included, and the binding."
#[test]
fn deleted_record_is_not_found_on_the_branch_and_after_commit() {
    let db = setup();
    let branch = db.begin().unwrap();
    db.delete(branch, RecordOp::delete(key(1), B)).ok();
    assert_err!(
        db.get(ReadTarget::Branch(branch), RecordOp::get(key(1), B))
            .result,
        ApiError::NotFound
    );
    // Still there at the head until the commit.
    read(&db, ReadTarget::Head, key(1));
    db.commit(branch).unwrap();
    assert_err!(
        db.get(ReadTarget::Head, RecordOp::get(key(1), B)).result,
        ApiError::NotFound
    );
}

/// "key not bound → `NotFound`"
#[test]
fn deleting_a_missing_or_deleted_record_is_not_found() {
    let db = setup();
    let branch = db.begin().unwrap();
    assert_err!(
        db.delete(branch, RecordOp::delete(key(2), B)).result,
        ApiError::NotFound
    );
    db.delete(branch, RecordOp::delete(key(1), B)).ok();
    assert_err!(
        db.delete(branch, RecordOp::delete(key(1), B)).result,
        ApiError::NotFound
    );
}

/// A deleted record cannot be patched.
#[test]
fn patching_a_deleted_record_is_not_found() {
    let db = setup();
    let branch = db.begin().unwrap();
    db.delete(branch, RecordOp::delete(key(1), B)).ok();
    assert_err!(
        db.patch(
            branch,
            RecordOp::patch(key(1), B).field("a", CellValue::from_u32(2))
        )
        .result,
        ApiError::NotFound
    );
}

/// Re-creating a deleted key starts from an empty record: no old cell survives.
#[test]
fn recreated_record_has_none_of_the_old_cells() {
    let db = setup();
    let branch = db.begin().unwrap();
    db.delete(branch, RecordOp::delete(key(1), B)).ok();
    db.create(
        branch,
        RecordOp::create(key(1), B).field("g", CellValue::from_u32(2)),
    )
    .ok();
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Branch(branch), key(1))),
        cells([("g", CellValue::from_u32(2))])
    );
    db.commit(branch).unwrap();
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Head, key(1))),
        cells([("g", CellValue::from_u32(2))])
    );
}

/// Create then delete in one branch leaves nothing.
#[test]
fn create_then_delete_in_one_branch_leaves_nothing() {
    let db = db();
    in_commit(&db, |b| {
        db.create(
            b,
            RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
        )
        .ok();
        db.delete(b, RecordOp::delete(key(1), B)).ok();
    });
    assert_err!(
        db.get(ReadTarget::Head, RecordOp::get(key(1), B)).result,
        ApiError::NotFound
    );
    let bindings = read(&db, ReadTarget::Head, reserved_key("#recordKeys"));
    assert_eq!(bindings.cells.get(key(1).0.as_slice()), None);
}

/// Deleting one record leaves others untouched.
#[test]
fn delete_touches_only_its_record() {
    let db = setup();
    commit_create(
        &db,
        RecordOp::create(key(2), B).attribute("a", CellValue::from_u32(1)),
    );
    in_commit(&db, |b| db.delete(b, RecordOp::delete(key(1), B)).ok());
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Head, key(2))),
        cells([("a", attr(CellValue::from_u32(1)))])
    );
}
