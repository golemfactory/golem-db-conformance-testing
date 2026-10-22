//! Data model: Cell names.
//!
//! ```text
//! name  = ["$"] first *rest        ; 1 .. #maxCellNameLen bytes, "$" included
//! first = ALPHA
//! rest  = ALPHA / DIGIT / "_" / "-" / "." / ":"
//! ```

use golemdb_conformance::*;

const VALID: &[&str] = &[
    "a",
    "Z",
    "$a",
    "a1",
    "a_b-c.d:e",
    "$A9_-.:",
    "abcdefghijklmnopqrstuvwxyzABCDEF", // 32 bytes: the limit
    "$bcdefghijklmnopqrstuvwxyzABCDEF", // 32 bytes, "$" included
];

const INVALID: &[&str] = &[
    "",
    "1a",
    "_a",
    "-a",
    ".a",
    ":a",
    "$",
    "$$a",
    "$1",
    "a$",
    "a b",
    "a/b",
    "a\0b",
    "é",
    "aé",
    "#a",
    "@a",
    "abcdefghijklmnopqrstuvwxyzABCDEFG", // 33 bytes
    "$bcdefghijklmnopqrstuvwxyzABCDEFG", // 33 bytes, "$" included
];

#[test]
fn valid_names_are_accepted_and_read_back() {
    let db = db();
    let mut op = RecordOp::create(key(1), B);
    for (i, name) in VALID.iter().enumerate() {
        op = op.field(name, CellValue::from_u32(i as u32));
    }
    commit_create(&db, op);
    let record = user_cells(&read(&db, ReadTarget::Head, key(1)));
    for (i, name) in VALID.iter().enumerate() {
        assert_eq!(
            record.get(*name),
            Some(&CellValue::from_u32(i as u32)),
            "{name}"
        );
    }
}

/// Every write step rejects a name outside the grammar or over `#maxCellNameLen`.
#[test]
fn invalid_names_are_invalid_argument_in_every_write_step() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let branch = db.begin().unwrap();
    for name in INVALID {
        let v = || CellValue::from_u32(1);
        let calls = [
            (
                "create.field",
                db.create(branch, RecordOp::create(key(2), B).field(name, v()))
                    .result
                    .map(drop),
            ),
            (
                "create.attribute",
                db.create(branch, RecordOp::create(key(2), B).attribute(name, v()))
                    .result
                    .map(drop),
            ),
            (
                "patch.field",
                db.patch(branch, RecordOp::patch(key(1), B).field(name, v()))
                    .result,
            ),
            (
                "patch.attribute",
                db.patch(branch, RecordOp::patch(key(1), B).attribute(name, v()))
                    .result,
            ),
            (
                "patch.set",
                db.patch(branch, RecordOp::patch(key(1), B).set(name, v()))
                    .result,
            ),
            (
                "patch.remove",
                db.patch(branch, RecordOp::patch(key(1), B).remove(name))
                    .result,
            ),
        ];
        for (step, result) in calls {
            assert!(
                matches!(result, Err(ApiError::InvalidArgument { .. })),
                "{step}({name:?}): expected InvalidArgument, got {result:?}"
            );
        }
    }
    // Nothing was written.
    assert_err!(
        db.get(ReadTarget::Branch(branch), RecordOp::get(key(2), B))
            .result,
        ApiError::NotFound
    );
}

/// "case-sensitive"
#[test]
fn names_are_case_sensitive() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .field("name", CellValue::from_u32(1))
            .field("Name", CellValue::from_u32(2)),
    );
    let record = read(&db, ReadTarget::Head, key(1));
    assert_eq!(
        user_cells(&record),
        cells([
            ("Name", CellValue::from_u32(2)),
            ("name", CellValue::from_u32(1))
        ])
    );
}

/// "`$` is ordinary user syntax": `$a` and `a` are different cells.
#[test]
fn dollar_prefix_is_part_of_the_name() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .field("a", CellValue::from_u32(1))
            .field("$a", CellValue::from_u32(2)),
    );
    let record = read(&db, ReadTarget::Head, key(1));
    assert_eq!(cell(&record, "a"), Some(&CellValue::from_u32(1)));
    assert_eq!(cell(&record, "$a"), Some(&CellValue::from_u32(2)));
}

/// "`#` and `@` start reserved names": a user cannot write or remove `#key` or `#meta`.
#[test]
fn system_cells_cannot_be_written_or_removed() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let branch = db.begin().unwrap();
    for name in ["#key", "#meta"] {
        assert_err!(
            db.patch(
                branch,
                RecordOp::patch(key(1), B).field(name, CellValue::from_bytes32([0; 32]))
            )
            .result,
            ApiError::InvalidArgument { .. }
        );
        assert_err!(
            db.patch(branch, RecordOp::patch(key(1), B).remove(name))
                .result,
            ApiError::InvalidArgument { .. }
        );
    }
    let record = read(&db, ReadTarget::Branch(branch), key(1));
    assert_eq!(
        cell(&record, "#key"),
        Some(&CellValue::from_bytes32(key(1).0))
    );
}
