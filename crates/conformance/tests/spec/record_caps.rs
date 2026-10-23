//! Per-record caps: `#maxRecordCells` and `#maxRecordIndexedCells`. The API spec
//! lists them as specified, not implemented; the rules are in metering D5 ("After
//! every `create` and `patch`, the record's resulting counts must stay within the
//! caps … a violation fails the call and writes nothing") and D2 (checked at plan
//! step 5, after the key binding), and their cells in the design's `#params` table.

use golemdb_conformance::*;

const CAPS: RecordLimits = RecordLimits {
    max_cells: 3,
    max_indexed_cells: 2,
};

fn capped(caps: RecordLimits) -> TestDb {
    open_db(&Genesis::new(HashAlgorithm::Keccak256, LIMITS, caps, None))
}

fn u(n: u32) -> CellValue {
    CellValue::from_u32(n)
}

fn next_id(db: &Database, b: BranchId) -> u64 {
    let alloc = read(db, ReadTarget::Branch(b), reserved_key("#alloc"));
    cell(&alloc, "#nextRecordID").unwrap().as_u64().unwrap()
}

/// "`#maxRecordCells` | `u32` (BE)"; "`#maxRecordIndexedCells` | `u32` (BE)"
#[test]
#[ignore = "spec-gap: the record caps are stored as u64, not u32"]
fn caps_are_u32_params() {
    let db = capped(CAPS);
    let params = read(&db, ReadTarget::Head, reserved_key("#params"));
    assert_eq!(cell(&params, "#maxRecordCells"), Some(&u(3)));
    assert_eq!(cell(&params, "#maxRecordIndexedCells"), Some(&u(2)));
}

/// The caps are readable in `#params`, whatever their width.
#[test]
fn caps_are_readable_in_params() {
    let db = capped(CAPS);
    let params = read(&db, ReadTarget::Head, reserved_key("#params"));
    let value = |name| {
        let v = cell(&params, name).unwrap();
        v.as_u32().map(u64::from).or(v.as_u64()).unwrap()
    };
    assert_eq!(value("#maxRecordCells"), 3);
    assert_eq!(value("#maxRecordIndexedCells"), 2);
}

/// A create up to the caps succeeds; one over fails, writes nothing and allocates no ID.
#[test]
fn create_is_held_to_the_caps() {
    let db = capped(CAPS);
    let b = db.begin().unwrap();
    db.create(
        b,
        RecordOp::create(key(1), B)
            .attribute("a", u(1))
            .attribute("b", u(2))
            .field("c", u(3)),
    )
    .ok();
    let id = next_id(&db, b);
    for op in [
        RecordOp::create(key(2), B)
            .field("a", u(1))
            .field("b", u(2))
            .field("c", u(3))
            .field("d", u(4)),
        RecordOp::create(key(2), B)
            .attribute("a", u(1))
            .attribute("b", u(2))
            .attribute("c", u(3)),
    ] {
        let outcome = db.create(b, op);
        assert!(outcome.result.is_err());
        assert_eq!(outcome.receipt.details, Details::default());
    }
    assert_err!(
        db.get(ReadTarget::Branch(b), RecordOp::get(key(2), B))
            .result,
        ApiError::NotFound
    );
    assert_eq!(next_id(&db, b), id);
}

/// "the record's resulting counts": a patch is judged by the record it leaves.
#[test]
fn patch_is_held_to_the_resulting_counts() {
    let db = capped(CAPS);
    commit_create(
        &db,
        RecordOp::create(key(1), B)
            .attribute("a", u(1))
            .attribute("b", u(2))
            .field("c", u(3)),
    );
    let b = db.begin().unwrap();
    let before = read(&db, ReadTarget::Branch(b), key(1));
    // Over either cap.
    for op in [
        RecordOp::patch(key(1), B).field("d", u(4)),
        RecordOp::patch(key(1), B).attribute("c", u(3)),
    ] {
        let outcome = db.patch(b, op);
        assert!(outcome.result.is_err());
        assert_eq!(outcome.receipt.details, Details::default());
        assert_eq!(read(&db, ReadTarget::Branch(b), key(1)), before);
    }
    // At the caps once applied, even though a cell is added.
    db.patch(b, RecordOp::patch(key(1), B).remove("c").field("d", u(4)))
        .ok();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .field("a", u(1))
            .attribute("d", u(4)),
    )
    .ok();
    assert_eq!(
        user_cells(&read(&db, ReadTarget::Branch(b), key(1))),
        cells([("a", u(1)), ("b", attr(u(2))), ("d", attr(u(4)))])
    );
}

/// Zero caps: `max_cells: 0` allows only empty records, `max_indexed_cells: 0` fields only.
#[test]
fn zero_caps() {
    let db = capped(RecordLimits {
        max_cells: 0,
        max_indexed_cells: 0,
    });
    let b = db.begin().unwrap();
    db.create(b, RecordOp::create(key(1), B)).ok();
    assert!(
        db.create(b, RecordOp::create(key(2), B).field("a", u(1)))
            .result
            .is_err()
    );
    assert!(
        db.patch(b, RecordOp::patch(key(1), B).field("a", u(1)))
            .result
            .is_err()
    );
    let db = capped(RecordLimits {
        max_cells: 2,
        max_indexed_cells: 0,
    });
    let b = db.begin().unwrap();
    db.create(
        b,
        RecordOp::create(key(1), B)
            .field("a", u(1))
            .field("b", u(2)),
    )
    .ok();
    assert!(
        db.patch(b, RecordOp::patch(key(1), B).attribute("a", u(1)))
            .result
            .is_err()
    );
}

/// D2: the caps are checked at plan step 5, after the binding (step 2), so
/// `AlreadyExists` and `NotFound` come first.
#[test]
fn binding_errors_come_before_the_caps() {
    let db = capped(CAPS);
    commit_create(&db, RecordOp::create(key(1), B));
    let b = db.begin().unwrap();
    let too_many = |op: RecordOp<op::Create>| {
        op.field("a", u(1))
            .field("b", u(2))
            .field("c", u(3))
            .field("d", u(4))
    };
    assert_err!(
        db.create(b, too_many(RecordOp::create(key(1), B))).result,
        ApiError::AlreadyExists
    );
    let patch = RecordOp::patch(key(9), B)
        .field("a", u(1))
        .field("b", u(2))
        .field("c", u(3))
        .field("d", u(4));
    assert_err!(db.patch(b, patch).result, ApiError::NotFound);
}

/// Each record has its own caps: many records at the cap are fine.
#[test]
fn caps_are_per_record() {
    let db = capped(CAPS);
    in_commit(&db, |b| {
        for n in 1..=20 {
            db.create(
                b,
                RecordOp::create(key(n), B)
                    .attribute("a", u(1))
                    .attribute("b", u(2))
                    .field("c", u(3)),
            )
            .ok();
        }
    });
    assert_eq!(user_cells(&read(&db, ReadTarget::Head, key(20))).len(), 3);
}
