//! Record operations: `patch`, row by row of its effect table.

use golemdb_conformance::*;

fn setup() -> (TestDb, BranchId) {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", CellValue::from_u32(1))
            .field("f", CellValue::from_str("x")),
    );
    let branch = db.begin().unwrap();
    (db, branch)
}

fn after(db: &Database, branch: BranchId) -> std::collections::BTreeMap<String, CellValue> {
    user_cells(&read(db, ReadTarget::Branch(branch), key(1)))
}

/// "missing | `attribute` / `field` | create"
#[test]
fn writing_a_missing_cell_creates_it() {
    let (db, b) = setup();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .attribute("new_a", CellValue::from_i64(-5))
            .field("new_f", CellValue::from_bool(false)),
    )
    .ok();
    assert_eq!(
        after(&db, b),
        cells([
            ("a", attr(CellValue::from_u32(1))),
            ("f", CellValue::from_str("x")),
            ("new_a", attr(CellValue::from_i64(-5))),
            ("new_f", CellValue::from_bool(false)),
        ])
    );
}

/// "present, equal to the new value (type, kind and payload) | **no-op**: nothing written or counted"
#[test]
fn writing_an_equal_value_is_a_no_op() {
    let (db, b) = setup();
    let version = db.branch_info(b).unwrap().version;
    let receipt = db
        .patch(
            b,
            RecordOp::patch(key(1), B)
                .attribute("a", CellValue::from_u32(1))
                .field("f", CellValue::from_str("x")),
        )
        .receipt;
    assert_eq!(receipt.details, Details::default());
    assert_eq!(db.branch_info(b).unwrap().version, version);
}

/// "present, different | update" — a different kind alone is different.
#[test]
fn writing_a_different_value_type_or_kind_updates() {
    let (db, b) = setup();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .field("a", CellValue::from_u32(1)) // same type and payload, now a field
            .attribute("f", CellValue::from_str("x")), // same type and payload, now an attribute
    )
    .ok();
    assert_eq!(
        after(&db, b),
        cells([
            ("a", CellValue::from_u32(1)),
            ("f", attr(CellValue::from_str("x")))
        ])
    );
    db.patch(
        b,
        RecordOp::patch(key(1), B).field("a", CellValue::from_u64(1)),
    )
    .ok();
    assert_eq!(after(&db, b)["a"], CellValue::from_u64(1));
}

/// "present | remove | delete"
#[test]
fn removing_a_present_cell_deletes_it() {
    let (db, b) = setup();
    db.patch(b, RecordOp::patch(key(1), B).remove("a")).ok();
    assert_eq!(after(&db, b), cells([("f", CellValue::from_str("x"))]));
}

/// "missing | remove | no-op"
#[test]
fn removing_a_missing_cell_is_a_no_op() {
    let (db, b) = setup();
    let version = db.branch_info(b).unwrap().version;
    let receipt = db
        .patch(b, RecordOp::patch(key(1), B).remove("nope"))
        .receipt;
    assert_eq!(receipt.details, Details::default());
    assert_eq!(db.branch_info(b).unwrap().version, version);
}

/// "present | `set` | the new value with the stored cell's kind"
#[test]
fn set_keeps_the_stored_kind() {
    let (db, b) = setup();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .set("a", CellValue::from_u32(2))
            .set("f", attr(CellValue::from_str("y"))),
    )
    .ok();
    assert_eq!(
        after(&db, b),
        cells([
            ("a", attr(CellValue::from_u32(2))),
            ("f", CellValue::from_str("y"))
        ])
    );
}

/// "a no-op if type and payload are also unchanged, an update otherwise" —
/// `set` may change the type.
#[test]
fn set_can_change_the_type_and_is_a_no_op_when_unchanged() {
    let (db, b) = setup();
    let receipt = db
        .patch(
            b,
            RecordOp::patch(key(1), B).set("a", CellValue::from_u32(1)),
        )
        .receipt;
    assert_eq!(receipt.details, Details::default());
    db.patch(
        b,
        RecordOp::patch(key(1), B).set("a", CellValue::from_str("1")),
    )
    .ok();
    assert_eq!(after(&db, b)["a"], attr(CellValue::from_str("1")));
}

/// "missing | `set` | **`CellNotFound { name }`**; the whole patch fails and nothing is written"
#[test]
fn set_on_a_missing_cell_is_cell_not_found_and_writes_nothing() {
    let (db, b) = setup();
    let version = db.branch_info(b).unwrap().version;
    assert_err!(
        db.patch(
            b,
            RecordOp::patch(key(1), B)
                .field("added", CellValue::from_u32(9))
                .remove("a")
                .set("zz", CellValue::from_u32(1))
        )
        .result,
        ApiError::CellNotFound { name } if name.as_bytes() == b"zz"
    );
    assert_eq!(
        after(&db, b),
        cells([
            ("a", attr(CellValue::from_u32(1))),
            ("f", CellValue::from_str("x"))
        ])
    );
    assert_eq!(db.branch_info(b).unwrap().version, version);
}

/// "An empty patch is valid on an existing record."
#[test]
fn empty_patch_is_valid() {
    let (db, b) = setup();
    db.patch(b, RecordOp::patch(key(1), B)).ok();
    assert_eq!(after(&db, b).len(), 2);
}

/// "Record state: key not bound → `NotFound`" — also for an empty patch.
#[test]
fn patching_a_missing_record_is_not_found() {
    let (db, b) = setup();
    assert_err!(
        db.patch(b, RecordOp::patch(key(2), B)).result,
        ApiError::NotFound
    );
    assert_err!(
        db.patch(
            b,
            RecordOp::patch(key(2), B).field("a", CellValue::from_u32(1))
        )
        .result,
        ApiError::NotFound
    );
    assert_err!(
        db.get(ReadTarget::Branch(b), RecordOp::get(key(2), B))
            .result,
        ApiError::NotFound
    );
}

/// "The whole patch is one atomic branch operation": an invalid value in any step writes nothing.
#[test]
fn a_patch_with_one_bad_change_writes_nothing() {
    let (db, b) = setup();
    assert_err!(
        db.patch(
            b,
            RecordOp::patch(key(1), B)
                .field("a", CellValue::from_u32(2))
                .field("s", CellValue::from_str(&"a".repeat(65)))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    assert_eq!(after(&db, b)["a"], attr(CellValue::from_u32(1)));
}

/// Patched values are what a later commit and a head read see.
#[test]
fn patch_then_commit_reads_back_at_head() {
    let (db, b) = setup();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .attribute("a", CellValue::from_u32(7))
            .remove("f")
            .field("g", CellValue::from_bytes(b"\x00\x01")),
    )
    .ok();
    db.commit(b).unwrap();
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Head, key(1))),
        cells([
            ("a", attr(CellValue::from_u32(7))),
            ("g", CellValue::from_bytes(b"\x00\x01"))
        ])
    );
}

/// Successive patches in one branch compose.
#[test]
fn patches_in_one_branch_compose() {
    let (db, b) = setup();
    db.patch(
        b,
        RecordOp::patch(key(1), B).field("x", CellValue::from_u32(1)),
    )
    .ok();
    db.patch(
        b,
        RecordOp::patch(key(1), B).field("x", CellValue::from_u32(2)),
    )
    .ok();
    db.patch(b, RecordOp::patch(key(1), B).remove("x").remove("f"))
        .ok();
    db.patch(
        b,
        RecordOp::patch(key(1), B).field("x", CellValue::from_u32(3)),
    )
    .ok();
    assert_eq!(
        after(&db, b),
        cells([
            ("a", attr(CellValue::from_u32(1))),
            ("x", CellValue::from_u32(3))
        ])
    );
}
