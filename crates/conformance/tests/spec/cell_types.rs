//! Data model: Cell types and kinds.

use golemdb_conformance::*;

/// One value of every type, with its spec type ID, at boundaries where they exist.
fn every_type() -> Vec<(&'static str, u8, CellValue)> {
    let mut i256_min = [0; 32];
    i256_min[0] = 0x80;
    vec![
        ("bool", 1, CellValue::from_bool(true)),
        ("str", 2, CellValue::from_str("héllo")),
        ("bytes", 3, CellValue::from_bytes(&[0, 1, 255])),
        ("bytes20", 4, CellValue::from_bytes20([0xab; 20])),
        ("bytes4", 8, CellValue::from_bytes4([1, 2, 3, 4])),
        ("bytes8", 9, CellValue::from_bytes8([0xff; 8])),
        ("bytes16", 10, CellValue::from_bytes16([0; 16])),
        ("bytes32", 11, CellValue::from_bytes32([0x5a; 32])),
        ("u32", 12, CellValue::from_u32(u32::MAX)),
        ("u64", 13, CellValue::from_u64(u64::MAX)),
        ("u128", 14, CellValue::from_u128(u128::MAX)),
        ("u256", 15, CellValue::from_u256_be([0xff; 32])),
        ("i32", 16, CellValue::from_i32(i32::MIN)),
        ("i64", 17, CellValue::from_i64(-1)),
        ("i128", 18, CellValue::from_i128(i128::MIN)),
        ("i256", 19, CellValue::from_i256_be(i256_min)),
        ("dec32", 20, CellValue::from_dec32_unscaled(-12_345)),
        ("dec64", 21, CellValue::from_dec64_unscaled(i64::MAX)),
        ("dec128", 22, CellValue::from_dec128_unscaled(1)),
        ("dec256", 23, CellValue::from_dec256_unscaled_be([0xff; 32])),
        ("f32", 24, CellValue::from_f32(-0.0).unwrap()),
        ("f64", 25, CellValue::from_f64(f64::NEG_INFINITY).unwrap()),
        ("date32", 28, CellValue::from_date32(-719_162)),
        ("timestamp64", 29, CellValue::from_timestamp64(i64::MAX)),
    ]
}

/// "A cell's value is a type tag byte followed by the payload." Every type
/// reads back exactly, with its type ID from the table.
#[test]
fn every_type_round_trips_as_a_field() {
    let db = db();
    let mut op = RecordOp::create(key(1), B);
    for (name, _, value) in every_type() {
        op = op.field(name, value);
    }
    commit_create(&db, op);
    let record = read(&db, ReadTarget::Head, key(1));
    for (name, id, value) in every_type() {
        let stored = cell(&record, name).unwrap_or_else(|| panic!("{name} missing"));
        assert_eq!(stored, &value, "{name}");
        assert_eq!(stored.cell_type().id(), id, "{name} type ID");
        assert_eq!(stored.kind(), CellKind::Field, "{name}");
    }
}

/// "Kind is chosen per write": every type but `bytes` can be an attribute.
#[test]
fn every_type_but_bytes_round_trips_as_an_attribute() {
    let db = db();
    let mut op = RecordOp::create(key(1), B);
    for (name, _, value) in every_type().into_iter().filter(|(n, ..)| *n != "bytes") {
        op = op.attribute(name, value);
    }
    commit_create(&db, op);
    let record = read(&db, ReadTarget::Head, key(1));
    for (name, id, value) in every_type().into_iter().filter(|(n, ..)| *n != "bytes") {
        let stored = cell(&record, name).unwrap();
        assert_eq!(stored, &attr(value), "{name}");
        assert_eq!(stored.cell_type().id(), id, "{name} type ID");
        assert_eq!(stored.kind(), CellKind::Attribute, "{name}");
    }
}

/// "`bytes` | as-is, at most `#maxBytesLen` bytes; **field only**"
#[test]
fn bytes_as_attribute_is_invalid_argument() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let branch = db.begin().unwrap();
    assert_err!(
        db.create(
            branch,
            RecordOp::create(key(2), B).attribute("b", CellValue::from_bytes(b"x"))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    assert_err!(
        db.patch(
            branch,
            RecordOp::patch(key(1), B).attribute("b", CellValue::from_bytes(b"x"))
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
}

/// Float payloads keep their exact bits, except that "`-0.0` normalizes to
/// `+0.0` on encode" (design §3, Cell Kinds and Types); infinities are values.
#[test]
fn float_bits_are_preserved_and_negative_zero_is_normalized() {
    let db = db();
    let floats = [
        (0.0, 0.0),
        (-0.0, 0.0),
        (f64::INFINITY, f64::INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY),
        (f64::MIN_POSITIVE / 2.0, f64::MIN_POSITIVE / 2.0),
    ];
    let mut op = RecordOp::create(key(1), B).field("f32", CellValue::from_f32(-0.0).unwrap());
    for (i, (f, _)) in floats.iter().enumerate() {
        op = op.field(&format!("f{i}"), CellValue::from_f64(*f).unwrap());
    }
    commit_create(&db, op);
    let record = read(&db, ReadTarget::Head, key(1));
    for (i, (f, expected)) in floats.iter().enumerate() {
        let back = cell(&record, &format!("f{i}")).unwrap().as_f64().unwrap();
        assert_eq!(back.to_bits(), expected.to_bits(), "{f}");
    }
    let back = cell(&record, "f32").unwrap().as_f32().unwrap();
    assert_eq!(back.to_bits(), 0.0f32.to_bits());
}

/// "`str` | UTF-8, at most `#maxStrLen` bytes" — bytes, not characters.
#[test]
fn str_limit_counts_utf8_bytes() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    let branch = db.begin().unwrap();
    let ok = |s: &str| {
        db.patch(
            branch,
            RecordOp::patch(key(1), B).field("s", CellValue::from_str(s)),
        )
        .result
    };
    ok(&"a".repeat(64)).unwrap();
    ok(&"é".repeat(32)).unwrap(); // 64 bytes
    ok("").unwrap();
    assert_err!(ok(&"a".repeat(65)), ApiError::InvalidArgument { .. });
    assert_err!(ok(&"é".repeat(33)), ApiError::InvalidArgument { .. }); // 66 bytes, 33 chars
    assert_err!(
        ok(&format!("{}é", "a".repeat(63))),
        ApiError::InvalidArgument { .. }
    );
}

/// The `str` limit applies to attributes and to `create` as well.
#[test]
fn str_limit_applies_to_attributes_and_create() {
    let db = db();
    let branch = db.begin().unwrap();
    let long = CellValue::from_str(&"a".repeat(65));
    assert_err!(
        db.create(
            branch,
            RecordOp::create(key(1), B).attribute("s", long.clone())
        )
        .result,
        ApiError::InvalidArgument { .. }
    );
    assert_err!(
        db.create(branch, RecordOp::create(key(1), B).field("s", long))
            .result,
        ApiError::InvalidArgument { .. }
    );
    db.create(
        branch,
        RecordOp::create(key(1), B).attribute("s", CellValue::from_str(&"a".repeat(64))),
    )
    .ok();
}

/// "`bytes` | as-is, at most `#maxBytesLen` bytes"
#[test]
fn bytes_limit() {
    let db = db();
    let branch = db.begin().unwrap();
    let create = |n: u8, len: usize| {
        db.create(
            branch,
            RecordOp::create(key(n), B).field("b", CellValue::from_bytes(&vec![7; len])),
        )
        .result
    };
    create(1, 128).unwrap();
    create(2, 0).unwrap();
    assert_err!(create(3, 129), ApiError::InvalidArgument { .. });
    let record = read(&db, ReadTarget::Branch(branch), key(1));
    assert_eq!(cell(&record, "b").unwrap().as_bytes().unwrap().len(), 128);
}

/// "Two records may give the same name different types and kinds."
#[test]
fn one_name_can_have_different_types_and_kinds_in_different_records() {
    let db = db();
    in_commit(&db, |b| {
        db.create(
            b,
            RecordOp::create(key(1), B).attribute("x", CellValue::from_u32(1)),
        )
        .ok();
        db.create(
            b,
            RecordOp::create(key(2), B).field("x", CellValue::from_str("one")),
        )
        .ok();
        db.create(
            b,
            RecordOp::create(key(3), B).attribute("x", CellValue::from_i64(-1)),
        )
        .ok();
    });
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&attr(CellValue::from_u32(1)))
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(2)), "x"),
        Some(&CellValue::from_str("one"))
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(3)), "x"),
        Some(&attr(CellValue::from_i64(-1)))
    );
}

/// Kind is chosen per write, so a patch can change a cell's type and kind.
#[test]
fn a_patch_can_change_a_cells_type_and_kind() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).attribute("x", CellValue::from_u32(1)),
    );
    commit_patch(
        &db,
        RecordOp::patch(key(1), B).field("x", CellValue::from_str("now a str")),
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&CellValue::from_str("now a str"))
    );
    commit_patch(
        &db,
        RecordOp::patch(key(1), B).attribute("x", CellValue::from_str("now a str")),
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "x"),
        Some(&attr(CellValue::from_str("now a str")))
    );
}

/// "`f32`, `f64`; NaN fails"
#[test]
fn nan_is_not_a_value() {
    assert!(CellValue::from_f32(f32::NAN).is_err());
    assert!(CellValue::from_f64(f64::NAN).is_err());
}
