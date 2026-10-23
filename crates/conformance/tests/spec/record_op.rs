//! Record operations: `RecordOp`. "Builder steps never fail. The first problem
//! is kept and reported by the call as `InvalidArgument`, before the branch is touched."
//! (`validate()` and the inspection accessors are not on `main` yet.)

use golemdb_conformance::*;

fn message(error: ApiError) -> String {
    match error {
        ApiError::InvalidArgument { message, .. } => message,
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

fn setup() -> (TestDb, BranchId) {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("x", CellValue::from_u32(1)),
    );
    let branch = db.begin().unwrap();
    (db, branch)
}

/// "a name written or removed twice in one operation"
#[test]
fn a_name_twice_in_one_operation_is_invalid_argument() {
    let (db, b) = setup();
    let v = || CellValue::from_u32(2);
    assert_err!(
        db.create(
            b,
            RecordOp::create(key(2), B)
                .field("y", v())
                .attribute("y", v())
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    for op in [
        RecordOp::patch(key(1), B).field("x", v()).field("x", v()),
        RecordOp::patch(key(1), B).field("x", v()).remove("x"),
        RecordOp::patch(key(1), B).remove("x").remove("x"),
        RecordOp::patch(key(1), B).set("x", v()).attribute("x", v()),
    ] {
        assert_err!(db.patch(b, op).result, ApiError::InvalidArgument { .. });
    }
    assert_eq!(
        cell(&read(&db, ReadTarget::Branch(b), key(1)), "x"),
        Some(&CellValue::from_u32(1))
    );
}

/// "`remove("x"): the operation already writes or removes this cell`"
#[test]
fn duplicate_name_message_names_the_second_step() {
    let (db, b) = setup();
    let error = db
        .patch(
            b,
            RecordOp::patch(key(1), B)
                .field("x", CellValue::from_u32(2))
                .remove("x"),
        )
        .err();
    assert_eq!(
        message(error),
        r#"remove("x"): the operation already writes or removes this cell"#
    );
}

/// "`attribute("x y"): invalid cell name`, cause as source"
#[test]
fn invalid_name_message_names_the_step() {
    let (db, b) = setup();
    let error = db
        .create(
            b,
            RecordOp::create(key(2), B).attribute("x y", CellValue::from_u32(1)),
        )
        .err();
    assert_eq!(message(error), r#"attribute("x y"): invalid cell name"#);
}

/// "a value that … cannot take the kind (`bytes` as attribute)" → "`field("f"): invalid cell value`"
#[test]
fn invalid_value_message_names_the_step() {
    let (db, b) = setup();
    let error = db
        .create(
            b,
            RecordOp::create(key(2), B).attribute("f", CellValue::from_bytes(b"x")),
        )
        .err();
    assert_eq!(message(error), r#"attribute("f"): invalid cell value"#);
}

/// "The first problem is kept": with several bad steps, the error names the first.
#[test]
fn the_first_bad_step_is_reported() {
    let (db, b) = setup();
    let error = db
        .create(
            b,
            RecordOp::create(key(2), B)
                .field("ok", CellValue::from_u32(1))
                .field("first bad", CellValue::from_u32(1))
                .field("second bad", CellValue::from_u32(1)),
        )
        .err();
    let message = message(error);
    assert!(message.contains("first bad"), "{message}");
    assert!(!message.contains("second bad"), "{message}");
}

/// "The builder checks the name grammar only, not `#maxCellNameLen` or the
/// value limits; the call checks those." Either way the call reports `InvalidArgument`.
#[test]
fn limits_are_checked_by_the_call() {
    let (db, b) = setup();
    let op = RecordOp::create(key(2), B).field(&"a".repeat(33), CellValue::from_u32(1));
    assert_err!(db.create(b, op).result, ApiError::InvalidArgument { .. });
}
