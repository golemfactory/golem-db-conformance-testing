//! Data model: Records.

use golemdb_conformance::*;

/// "A record is its key, zero or more user cells, and two cells the database maintains."
#[test]
fn created_cells_read_back_with_value_and_kind() {
    let db = db();
    let k = commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("owner", CellValue::from_str("alice"))
            .field("size", CellValue::from_u64(42)),
    );
    assert_eq!(k, key(1));
    let record = read(&db, ReadTarget::Head, k);
    assert_eq!(record.key, key(1));
    assert_eq!(
        user_cells(&record),
        cells([
            ("owner", attr(CellValue::from_str("alice"))),
            ("size", CellValue::from_u64(42)),
        ])
    );
    assert_eq!(cell(&record, "owner").unwrap().kind(), CellKind::Attribute);
    assert_eq!(cell(&record, "size").unwrap().kind(), CellKind::Field);
}

/// "`#key` | the database | the record key, `bytes32` field"
#[test]
fn key_cell_holds_the_record_key_as_a_bytes32_field() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let record = read(&db, ReadTarget::Head, key(1));
    let key_cell = cell(&record, "#key").expect("#key is returned by a full read");
    assert_eq!(key_cell, &CellValue::from_bytes32(key(1).0));
    assert_eq!(key_cell.kind(), CellKind::Field);
}

/// "Empty records are legal: a create may name no cells"
#[test]
fn empty_record_can_be_created_and_read() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    assert!(user_cells(&read(&db, ReadTarget::Head, key(1))).is_empty());
}

/// "a patch may remove the last one. Only `delete` removes a record."
#[test]
fn removing_the_last_cell_leaves_an_empty_record() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("a", CellValue::from_u32(1)),
    );
    commit_patch(&db, RecordOp::patch(key(1), B).remove("a"));
    let record = read(&db, ReadTarget::Head, key(1));
    assert!(user_cells(&record).is_empty());
}

/// "`Record::cells` is ordered bytewise by name."
#[test]
fn cells_are_ordered_bytewise_by_name() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .field("b", CellValue::from_u32(1))
            .field("B", CellValue::from_u32(2))
            .field("a", CellValue::from_u32(3))
            .field("$z", CellValue::from_u32(4)),
    );
    let names: Vec<_> = user_cells(&read(&db, ReadTarget::Head, key(1)))
        .into_keys()
        .collect();
    assert_eq!(names, ["$z", "B", "a", "b"]);
}

/// Create, edit and read back across a reopen: only `commit` writes to the store.
#[test]
fn committed_record_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let config = OpenConfig::new(genesis());
    {
        let db = Database::open(dir.path(), &config).unwrap();
        commit_create(
            &db,
            RecordOp::create(key(1), B)
                .attribute("a", CellValue::from_u32(1))
                .field("b", CellValue::from_str("x")),
        );
        commit_patch(
            &db,
            RecordOp::patch(key(1), B)
                .attribute("a", CellValue::from_u32(2))
                .remove("b")
                .field("c", CellValue::from_bool(true)),
        );
        // Never committed: gone after reopen.
        let branch = db.begin().unwrap();
        db.create(branch, RecordOp::create(key(2), B)).ok();
    }
    let db = Database::open(dir.path(), &config).unwrap();
    assert_eq!(db.head().unwrap(), CommitId::new(2));
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Head, key(1))),
        cells([
            ("a", attr(CellValue::from_u32(2))),
            ("c", CellValue::from_bool(true)),
        ])
    );
    assert_err!(
        db.get(ReadTarget::Head, RecordOp::get(key(2), B)).result,
        ApiError::NotFound
    );
}
