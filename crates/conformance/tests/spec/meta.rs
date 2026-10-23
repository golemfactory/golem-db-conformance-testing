//! Data model: `#meta`. "four counts over its user cells": `cells`, `cell_bytes`,
//! `indexed_cells`, `index_bytes`, each `u64` BE in a `bytes32` field.

use golemdb_conformance::*;

fn expected(user: &[(&str, CellValue)]) -> [u64; 4] {
    let attrs = user.iter().filter(|(_, v)| v.kind() == CellKind::Attribute);
    [
        user.len() as u64,
        user.iter().map(|(n, v)| cell_size(n, v)).sum(),
        attrs.clone().count() as u64,
        attrs.map(|(n, v)| term_size(n, v)).sum(),
    ]
}

fn meta_of(db: &Database, target: ReadTarget) -> [u64; 4] {
    let record = db
        .get(target, RecordOp::get(key(1), B).only(["#meta"]))
        .ok();
    meta(&record).expect("#meta present")
}

/// "`create` writes it"; "a full `get` returns `#meta`; so does `.only(["#meta"])`"
#[test]
fn create_writes_meta() {
    let db = db();
    let user = [
        ("a", attr(CellValue::from_u32(1))),
        ("name", attr(CellValue::from_str("alice"))),
        ("blob", CellValue::from_bytes(&[1; 100])),
    ];
    let mut op = RecordOp::create(key(1), B);
    for (n, v) in &user {
        op = match v.kind() {
            CellKind::Attribute => op.attribute(n, v.clone()),
            CellKind::Field => op.field(n, v.clone()),
        };
    }
    commit_create(&db, op);
    assert_eq!(meta_of(&db, ReadTarget::Head), expected(&user));
    assert_eq!(
        meta(&read(&db, ReadTarget::Head, key(1))),
        Some(expected(&user))
    );
    assert_eq!(
        cell(&read(&db, ReadTarget::Head, key(1)), "#meta")
            .unwrap()
            .kind(),
        CellKind::Field
    );
}

/// An empty record has `#meta` with all counts zero.
#[test]
fn empty_record_has_zero_meta() {
    let db = db();
    commit_create(&db, RecordOp::create(key(1), B));
    assert_eq!(meta_of(&db, ReadTarget::Head), [0; 4]);
}

/// "`patch` writes it only when a count changes"; the counts follow every mutation.
#[test]
fn patch_keeps_meta_exact() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).attribute("a", CellValue::from_u32(1)),
    );
    let b = db.begin().unwrap();
    db.patch(
        b,
        RecordOp::patch(key(1), B)
            .field("a", CellValue::from_u64(2)) // attribute -> field, wider
            .attribute("s", CellValue::from_str("xyz")),
    )
    .ok();
    assert_eq!(
        meta_of(&db, ReadTarget::Branch(b)),
        expected(&[
            ("a", CellValue::from_u64(2)),
            ("s", attr(CellValue::from_str("xyz")))
        ])
    );
    db.patch(b, RecordOp::patch(key(1), B).remove("a").remove("s"))
        .ok();
    assert_eq!(meta_of(&db, ReadTarget::Branch(b)), [0; 4]);
}

/// "`delete` removes it."
#[test]
fn recreated_record_has_fresh_meta() {
    let db = db();
    commit_create(
        &db,
        RecordOp::create(key(1), B).field("a", CellValue::from_u32(1)),
    );
    let b = db.begin().unwrap();
    db.delete(b, RecordOp::delete(key(1), B)).ok();
    db.create(b, RecordOp::create(key(1), B)).ok();
    assert_eq!(meta_of(&db, ReadTarget::Branch(b)), [0; 4]);
}
