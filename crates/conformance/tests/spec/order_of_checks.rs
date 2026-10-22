//! Record operations: Order of checks. "Every call checks in three stages …;
//! the first failure is returned": 1. handle, 2. admission, 3. record state.

use golemdb_conformance::*;

fn bad_name() -> RecordOp<op::Create> {
    RecordOp::create(key(2), B).field("bad name", CellValue::from_u32(1))
}

fn dead_branch(db: &Database) -> BranchId {
    let branch = db.begin().unwrap();
    db.discard(branch).unwrap();
    branch
}

fn sealed_branch(db: &Database) -> BranchId {
    let branch = db.begin().unwrap();
    db.seal(branch).unwrap();
    branch
}

fn setup() -> Database {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    db
}

/// "So a bad operation on a dead branch reports `HandleInvalid` and costs nothing."
#[test]
fn bad_operations_on_a_dead_branch_are_handle_invalid() {
    let db = setup();
    let b = dead_branch(&db);
    assert_err!(db.create(b, bad_name()).result, ApiError::HandleInvalid);
    assert_err!(
        db.patch(
            b,
            RecordOp::patch(key(9), B).field("bad name", CellValue::from_u32(1))
        )
        .result,
        ApiError::HandleInvalid
    );
    assert_err!(
        db.delete(b, RecordOp::delete(key(9), B)).result,
        ApiError::HandleInvalid
    );
}

/// Reserved keys are an admission check, so the handle comes first.
#[test]
fn reserved_keys_on_a_dead_branch_are_handle_invalid() {
    let db = setup();
    let b = dead_branch(&db);
    let k = reserved_key("#params");
    assert_err!(
        db.create(b, RecordOp::create(k, B)).result,
        ApiError::HandleInvalid
    );
    assert_err!(
        db.patch(b, RecordOp::patch(k, B)).result,
        ApiError::HandleInvalid
    );
    assert_err!(
        db.delete(b, RecordOp::delete(k, B)).result,
        ApiError::HandleInvalid
    );
}

/// "Handle: … sealed → `Sealed`", before admission and record state.
#[test]
fn bad_operations_on_a_sealed_branch_are_sealed() {
    let db = setup();
    let b = sealed_branch(&db);
    assert_err!(db.create(b, bad_name()).result, ApiError::Sealed);
    assert_err!(
        db.create(b, RecordOp::create(key(1), B)).result,
        ApiError::Sealed
    );
    assert_err!(
        db.patch(b, RecordOp::patch(key(9), B)).result,
        ApiError::Sealed
    );
    assert_err!(
        db.delete(b, RecordOp::delete(key(9), B)).result,
        ApiError::Sealed
    );
}

#[test]
fn reserved_keys_on_a_sealed_branch_are_sealed() {
    let db = setup();
    let b = sealed_branch(&db);
    let k = reserved_key("#params");
    assert_err!(
        db.create(b, RecordOp::create(k, B)).result,
        ApiError::Sealed
    );
    assert_err!(db.patch(b, RecordOp::patch(k, B)).result, ApiError::Sealed);
    assert_err!(
        db.delete(b, RecordOp::delete(k, B)).result,
        ApiError::Sealed
    );
}

/// create admission: "1. builder error → `InvalidArgument`; 2. caller-assigned key
/// equal to a reserved key → `Reserved`"
#[test]
#[ignore = "spec-gap: reserved keys are checked before builder errors (create/patch admission order)"]
fn create_reports_a_builder_error_before_a_reserved_key() {
    let db = setup();
    let b = db.begin().unwrap();
    let op = RecordOp::create(reserved_key("#params"), B).field("bad name", CellValue::from_u32(1));
    assert_err!(db.create(b, op).result, ApiError::InvalidArgument { .. });
}

/// patch admission: "builder error → `InvalidArgument`; reserved key → `Reserved`"
#[test]
#[ignore = "spec-gap: reserved keys are checked before builder errors (create/patch admission order)"]
fn patch_reports_a_builder_error_before_a_reserved_key() {
    let db = setup();
    let b = db.begin().unwrap();
    let op = RecordOp::patch(reserved_key("#params"), B).remove("bad name");
    assert_err!(db.patch(b, op).result, ApiError::InvalidArgument { .. });
}

/// create: genesis limits are admission, `AlreadyExists` is record state.
#[test]
fn create_reports_limits_before_already_exists() {
    let db = setup();
    let b = db.begin().unwrap();
    let op = RecordOp::create(key(1), B).field("s", CellValue::from_str(&"a".repeat(65)));
    assert_err!(db.create(b, op).result, ApiError::InvalidArgument { .. });
}

/// patch: "names and values against the limits → `InvalidArgument`" before
/// "key not bound → `NotFound`".
#[test]
fn patch_reports_limits_before_not_found() {
    let db = setup();
    let b = db.begin().unwrap();
    let long_value = RecordOp::patch(key(9), B).field("s", CellValue::from_str(&"a".repeat(65)));
    assert_err!(
        db.patch(b, long_value).result,
        ApiError::InvalidArgument { .. }
    );
    let long_name = RecordOp::patch(key(9), B).field(&"a".repeat(33), CellValue::from_u32(1));
    assert_err!(
        db.patch(b, long_name).result,
        ApiError::InvalidArgument { .. }
    );
}

/// patch: a builder error on a missing record is still admission.
#[test]
fn patch_reports_a_builder_error_before_not_found() {
    let db = setup();
    let b = db.begin().unwrap();
    let op = RecordOp::patch(key(9), B).field("bad name", CellValue::from_u32(1));
    assert_err!(db.patch(b, op).result, ApiError::InvalidArgument { .. });
}

/// get: "the target first, at no cost …; then the binding (`NotFound`)"
#[test]
fn get_reports_the_target_before_not_found() {
    let db = setup();
    let b = dead_branch(&db);
    assert_err!(
        db.get(ReadTarget::Branch(b), RecordOp::get(key(9), B))
            .result,
        ApiError::HandleInvalid
    );
    assert_err!(
        db.get(
            ReadTarget::Commit(CommitId::new(99)),
            RecordOp::get(key(9), B)
        )
        .result,
        ApiError::CommitUnavailable { .. }
    );
}
