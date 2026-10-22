//! Receipts. "Every record call returns `Metered<T>`: its result together with
//! a receipt. The receipt is present on success and on failure".

use golemdb_conformance::*;

/// `Details` as `[cells_created, cells_updated, cells_deleted, index_joins,
/// index_leaves, cell_bytes_written, cell_bytes_deleted, index_bytes_written, index_bytes_deleted]`.
fn d(r: &Receipt) -> [u64; 9] {
    let d = r.details;
    [
        d.cells_created,
        d.cells_updated,
        d.cells_deleted,
        d.index_joins,
        d.index_leaves,
        d.cell_bytes_written,
        d.cell_bytes_deleted,
        d.index_bytes_written,
        d.index_bytes_deleted,
    ]
}

/// "create a cell | `cells_created`; `cell_bytes_written` += its size; if an
/// attribute, `index_joins` and `index_bytes_written`". System cells are not counted.
#[test]
fn create_counts_each_user_cell() {
    let db = db();
    let b = db.begin().unwrap();
    let a = attr(CellValue::from_str("alice"));
    let f = CellValue::from_bytes(&[0; 10]);
    let receipt = db
        .create(
            b,
            RecordOp::create(key(1), B)
                .attribute("owner", a.clone())
                .field("data", f.clone()),
        )
        .receipt;
    assert_eq!(
        d(&receipt),
        [
            2,
            0,
            0,
            1,
            0,
            cell_size("owner", &a) + cell_size("data", &f),
            0,
            term_size("owner", &a),
            0
        ]
    );
    // "Sizes are as for `#meta`": owner = (8+5) + (1+5), data = (8+4) + (1+10).
    assert_eq!(cell_size("owner", &a), 19);
    assert_eq!(cell_size("data", &f), 23);
    assert_eq!(term_size("owner", &a), 12);
}

#[test]
fn creating_an_empty_record_counts_nothing() {
    let db = db();
    let b = db.begin().unwrap();
    assert_eq!(
        d(&db.create(b, RecordOp::create(key(1), B)).receipt),
        [0; 9]
    );
}

/// "update a cell | `cells_updated`; old size to `cell_bytes_deleted`, new size to
/// `cell_bytes_written`; leave for an old attribute, join for a new one"
#[test]
fn update_counts_old_and_new_sizes() {
    let db = db();
    let old_a = attr(CellValue::from_u32(1));
    let old_f = CellValue::from_str("ab");
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", old_a.clone())
            .field("f", old_f.clone()),
    );
    let b = db.begin().unwrap();
    // attribute -> attribute: one leave and one join.
    let new_a = attr(CellValue::from_u64(2));
    let receipt = db
        .patch(b, RecordOp::patch(key(1), B).attribute("a", new_a.clone()))
        .receipt;
    assert_eq!(
        d(&receipt),
        [
            0,
            1,
            0,
            1,
            1,
            cell_size("a", &new_a),
            cell_size("a", &old_a),
            term_size("a", &new_a),
            term_size("a", &old_a)
        ]
    );
    // field -> attribute: a join only.
    let new_f = attr(CellValue::from_str("abc"));
    let receipt = db
        .patch(b, RecordOp::patch(key(1), B).attribute("f", new_f.clone()))
        .receipt;
    assert_eq!(
        d(&receipt),
        [
            0,
            1,
            0,
            1,
            0,
            cell_size("f", &new_f),
            cell_size("f", &old_f),
            term_size("f", &new_f),
            0
        ]
    );
    // attribute -> field: a leave only.
    let field_a = CellValue::from_u64(2);
    let receipt = db
        .patch(b, RecordOp::patch(key(1), B).field("a", field_a.clone()))
        .receipt;
    assert_eq!(
        d(&receipt),
        [
            0,
            1,
            0,
            0,
            1,
            cell_size("a", &field_a),
            cell_size("a", &new_a),
            0,
            term_size("a", &new_a)
        ]
    );
}

/// "delete a cell | `cells_deleted`; `cell_bytes_deleted`; if an attribute, `index_leaves` and `index_bytes_deleted`"
#[test]
fn remove_and_delete_count_deleted_cells() {
    let db = db();
    let a = attr(CellValue::from_i32(-1));
    let f = CellValue::from_bool(true);
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", a.clone())
            .field("f", f.clone()),
    );
    let b = db.begin().unwrap();
    let removed = db.patch(b, RecordOp::patch(key(1), B).remove("a")).receipt;
    assert_eq!(
        d(&removed),
        [0, 0, 1, 0, 1, 0, cell_size("a", &a), 0, term_size("a", &a)]
    );
    let deleted = db.delete(b, RecordOp::delete(key(1), B)).receipt;
    assert_eq!(d(&deleted), [0, 0, 1, 0, 0, 0, cell_size("f", &f), 0, 0]);
}

/// "Effects, not requests: a no-op counts nothing. Failed calls and reads report all zeros."
#[test]
fn failed_calls_and_reads_count_nothing() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
    );
    let b = db.begin().unwrap();
    let receipts = [
        db.create(
            b,
            RecordOp::create(key(1), B).field("x", CellValue::from_u32(1)),
        )
        .receipt, // AlreadyExists
        db.patch(
            b,
            RecordOp::patch(key(1), B)
                .field("x", CellValue::from_u32(1))
                .set("y", CellValue::from_u32(1)),
        )
        .receipt, // CellNotFound
        db.patch(
            b,
            RecordOp::patch(key(1), B).attribute("a", CellValue::from_u32(1)),
        )
        .receipt, // no-op
        db.delete(b, RecordOp::delete(key(9), B)).receipt, // NotFound
        db.get(ReadTarget::Head, RecordOp::get(key(1), B)).receipt,
        db.get(ReadTarget::Branch(b), RecordOp::get(key(1), B))
            .receipt,
    ];
    for (i, receipt) in receipts.iter().enumerate() {
        assert_eq!(d(receipt), [0; 9], "call {i}");
    }
}

/// "**`cost`** is always 0 until metering is implemented."
#[test]
fn cost_is_zero() {
    let db = db();
    let b = db.begin().unwrap();
    let receipt = db
        .create(
            b,
            RecordOp::create(key(1), B).field("a", CellValue::from_u32(1)),
        )
        .receipt;
    assert_eq!(receipt.cost, 0);
}

/// "write, or `get` on a branch | the branch's origin commit"
#[test]
fn writes_and_branch_reads_are_priced_at_the_origin() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let b = db.begin().unwrap();
    let origin = Some(CommitId::new(1));
    assert_eq!(
        db.create(b, RecordOp::create(key(2), B)).receipt.priced_at,
        origin
    );
    assert_eq!(
        db.patch(b, RecordOp::patch(key(1), B)).receipt.priced_at,
        origin
    );
    assert_eq!(
        db.get(ReadTarget::Branch(b), RecordOp::get(key(1), B))
            .receipt
            .priced_at,
        origin
    );
    assert_eq!(
        db.delete(b, RecordOp::delete(key(1), B)).receipt.priced_at,
        origin
    );
    // Failures too.
    assert_eq!(
        db.delete(b, RecordOp::delete(key(9), B)).receipt.priced_at,
        origin
    );
}

/// "`get` on `Head` | the head of the snapshot read"; "`get` on `Commit(c)`,
/// success or failure | the head at admission, not `c`"
#[test]
fn committed_reads_are_priced_at_the_head() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    commit_create(&db, RecordOp::create(key(2), B));
    let head = Some(CommitId::new(2));
    assert_eq!(
        db.get(ReadTarget::Head, RecordOp::get(key(1), B))
            .receipt
            .priced_at,
        head
    );
    assert_eq!(
        db.get(ReadTarget::Head, RecordOp::get(key(9), B))
            .receipt
            .priced_at,
        head
    );
    let at_head = db.get(
        ReadTarget::Commit(CommitId::new(2)),
        RecordOp::get(key(1), B),
    );
    assert_eq!(at_head.receipt.priced_at, head);
    let old = db.get(
        ReadTarget::Commit(CommitId::new(1)),
        RecordOp::get(key(1), B),
    );
    assert_err!(old.result, ApiError::CommitUnavailable { .. });
    assert_eq!(old.receipt.priced_at, head);
}

/// "unknown or consumed handle … | `None`: no snapshot was captured, and the call costs 0"
#[test]
fn calls_on_dead_handles_are_priced_at_none() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let b = db.begin().unwrap();
    db.discard(b).unwrap();
    let receipts = [
        db.create(b, RecordOp::create(key(2), B)).receipt,
        db.patch(b, RecordOp::patch(key(1), B)).receipt,
        db.delete(b, RecordOp::delete(key(1), B)).receipt,
        db.get(ReadTarget::Branch(b), RecordOp::get(key(1), B))
            .receipt,
    ];
    for (i, receipt) in receipts.iter().enumerate() {
        assert_eq!(receipt.priced_at, None, "call {i}");
        assert_eq!(receipt.cost, 0, "call {i}");
    }
}
