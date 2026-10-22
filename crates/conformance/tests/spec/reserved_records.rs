//! Data model: Reserved records.

use golemdb_conformance::*;

const RESERVED: [&str; 7] = [
    "#params",
    "#alloc",
    "#roots",
    "#recordKeys",
    "#rootIndex",
    "@meteringModel",
    "@modelWeight",
];

/// "`create`, `patch` and `delete` on a reserved key return `Reserved`."
#[test]
fn writes_to_reserved_keys_are_reserved() {
    let db = db();
    let branch = db.begin().unwrap();
    for name in RESERVED {
        let k = reserved_key(name);
        assert_err!(
            db.create(branch, RecordOp::create(k, B)).result,
            ApiError::Reserved
        );
        assert_err!(
            db.patch(
                branch,
                RecordOp::patch(k, B).field("a", CellValue::from_u32(1))
            )
            .result,
            ApiError::Reserved
        );
        assert_err!(
            db.delete(branch, RecordOp::delete(k, B)).result,
            ApiError::Reserved
        );
    }
}

/// "`get` works on reserved records like on any other; a full read returns all their cells."
#[test]
fn params_record_holds_the_genesis_limits() {
    let db = db();
    let params = read(&db, ReadTarget::Head, reserved_key("#params"));
    assert_eq!(
        cell(&params, "#maxCellNameLen"),
        Some(&CellValue::from_u32(32))
    );
    assert_eq!(cell(&params, "#maxStrLen"), Some(&CellValue::from_u32(64)));
    assert_eq!(
        cell(&params, "#maxBytesLen"),
        Some(&CellValue::from_u32(128))
    );
    assert_eq!(
        cell(&params, "#key"),
        Some(&CellValue::from_bytes32(reserved_key("#params").0))
    );
}

/// "`#params`: … `#keyMode` (`u32`: 0 caller-assigned, 1 generated)"
#[test]
#[ignore = "spec-gap: record key modes (#keyMode in #params) not implemented"]
fn params_record_holds_the_key_mode() {
    let db = db();
    let params = read(&db, ReadTarget::Head, reserved_key("#params"));
    assert_eq!(cell(&params, "#keyMode"), Some(&CellValue::from_u32(0)));
}

/// "`#alloc`: `#nextRecordID` = 64 (`u64`)"; user records start at 64 and each create advances it.
#[test]
fn alloc_record_counts_allocated_records() {
    let db = db();
    let next = |db: &Database| {
        let alloc = read(db, ReadTarget::Head, reserved_key("#alloc"));
        cell(&alloc, "#nextRecordID").unwrap().as_u64().unwrap()
    };
    assert_eq!(next(&db), 64);
    commit_create(&db, RecordOp::create(key(1), B));
    commit_create(&db, RecordOp::create(key(2), B));
    assert_eq!(next(&db), 66);
    // "The allocator does not move back; re-creating the key gives a new ID."
    in_commit(&db, |b| {
        db.delete(b, RecordOp::delete(key(1), B)).ok();
        db.create(b, RecordOp::create(key(1), B)).ok();
    });
    assert_eq!(next(&db), 67);
}

/// "`#recordKeys` | bindings: name = raw record key, value = record ID (`u64`)"
#[test]
fn record_keys_record_binds_each_key_to_its_id() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let bindings = read(&db, ReadTarget::Head, reserved_key("#recordKeys"));
    assert_eq!(
        bindings.cells.get(key(1).0.as_slice()),
        Some(&CellValue::from_u64(64))
    );
    in_commit(&db, |b| db.delete(b, RecordOp::delete(key(1), B)).ok());
    let bindings = read(&db, ReadTarget::Head, reserved_key("#recordKeys"));
    assert_eq!(bindings.cells.get(key(1).0.as_slice()), None);
}

/// "`#roots` | one cell per earlier commit: name = `commitNr` (`u64` BE), value =
/// `StateRoot ‖ IndexRoot` (64-byte `bytes` field), written lag-one by each commit"
#[test]
fn roots_record_holds_each_earlier_commits_roots() {
    let db = db();
    let genesis = *db.info();
    let mut sealed = Vec::new();
    for n in 1..=3u8 {
        let branch = db.begin().unwrap();
        db.create(branch, RecordOp::create(key(n), B)).ok();
        sealed.push(db.seal(branch).unwrap());
        db.commit(branch).unwrap();
    }
    let roots = read(&db, ReadTarget::Head, reserved_key("#roots"));
    let root_of = |commit: u64| roots.cells.get(commit.to_be_bytes().as_slice());
    let expected =
        |state: [u8; 32], index: [u8; 32]| CellValue::from_bytes(&[state, index].concat());
    assert_eq!(
        root_of(0),
        Some(&expected(genesis.state_root, genesis.index_root))
    );
    assert_eq!(
        root_of(1),
        Some(&expected(sealed[0].state_root, sealed[0].index_root))
    );
    assert_eq!(
        root_of(2),
        Some(&expected(sealed[1].state_root, sealed[1].index_root))
    );
    // Lag-one: the head's own roots are not in it yet.
    assert_eq!(root_of(3), None);
}

/// "Each reserved record's key is its name, ASCII, zero-padded to 32 bytes."
#[test]
fn user_keys_resembling_reserved_names_are_ordinary() {
    let db = db();
    // Not zero-padded, so not a reserved key.
    let mut k = [b' '; 32];
    k[..7].copy_from_slice(b"#params");
    commit_create(
        &db,
        RecordOp::create(RecordKey(k), B).field("a", CellValue::from_u32(1)),
    );
    let record = read(&db, ReadTarget::Head, RecordKey(k));
    assert_eq!(user_cells(&record), cells([("a", CellValue::from_u32(1))]));
}
