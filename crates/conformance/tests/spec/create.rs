//! Record operations: `create`.

use golemdb_conformance::*;

/// "On success it writes the user cells, `#key`, `#meta`, the binding and the
/// advanced allocator as one atomic branch operation, and returns the key."
#[test]
fn create_returns_the_key_and_is_visible_on_its_branch_only() {
    let db = db();
    let branch = db.begin().unwrap();
    let k = db
        .create(
            branch,
            RecordOp::create(key(1), B).field("a", CellValue::from_u32(1)),
        )
        .ok();
    assert_eq!(k, key(1));
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Branch(branch), k)),
        cells([("a", CellValue::from_u32(1))])
    );
    assert_err!(
        db.get(ReadTarget::Head, RecordOp::get(k, B)).result,
        ApiError::NotFound
    );
    let other = db.begin().unwrap();
    assert_err!(
        db.get(ReadTarget::Branch(other), RecordOp::get(k, B))
            .result,
        ApiError::NotFound
    );
}

/// "the first failure is returned and nothing is written": a failing create
/// leaves no binding, cells or allocator change behind.
#[test]
fn a_failed_create_writes_nothing() {
    let db = db();
    let branch = db.begin().unwrap();
    let before = db.branch_info(branch).unwrap().version;
    assert_err!(
        db.create(
            branch,
            RecordOp::create(key(1), B)
                .field("ok", CellValue::from_u32(1))
                .field("s", CellValue::from_str(&"a".repeat(65)))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    assert_err!(
        db.get(ReadTarget::Branch(branch), RecordOp::get(key(1), B))
            .result,
        ApiError::NotFound
    );
    assert_eq!(db.branch_info(branch).unwrap().version, before);
    // The key is still free.
    db.create(branch, RecordOp::create(key(1), B)).ok();
    let alloc = read(&db, ReadTarget::Branch(branch), reserved_key("#alloc"));
    assert_eq!(
        cell(&alloc, "#nextRecordID"),
        Some(&CellValue::from_u64(65))
    );
}

/// "Order of checks … 2. Admission … 4. each name against `#maxCellNameLen` and each
/// `str` / `bytes` value against its limit, in name order → `InvalidArgument`.
/// 3. Record state … key already bound → `AlreadyExists`": admission comes first.
#[test]
fn admission_errors_come_before_already_exists() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let branch = db.begin().unwrap();
    assert_err!(
        db.create(
            branch,
            RecordOp::create(key(1), B).field("s", CellValue::from_str(&"a".repeat(65)))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    assert_err!(
        db.create(
            branch,
            RecordOp::create(key(1), B).field("1bad", CellValue::from_u32(1))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
}

/// Records in one branch are independent: many creates, each reads back its own cells.
#[test]
fn many_records_in_one_commit_read_back_independently() {
    let db = db();
    in_commit(&db, |b| {
        for n in 1..=100u8 {
            db.create(
                b,
                RecordOp::create(key(n), B)
                    .attribute("n", CellValue::from_u32(n as u32))
                    .field("label", CellValue::from_str(&format!("record {n}"))),
            )
            .ok();
        }
    });
    for n in 1..=100u8 {
        assert_eq!(
            user_cells(&read(&db, ReadTarget::Head, key(n))),
            cells([
                ("label", CellValue::from_str(&format!("record {n}"))),
                ("n", attr(CellValue::from_u32(n as u32))),
            ])
        );
    }
}
