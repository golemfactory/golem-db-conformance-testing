//! Query and count: "Specified, not yet implemented → Query" in the API spec,
//! detailed by `docs/query-proposal.md` (the call, the AST, execution rules,
//! errors, staging). `Not`, `Exists`, the cursor and the `#params` caps are not
//! on `main` and do not compile against it yet.

use std::ops::Bound::{self, Excluded, Included, Unbounded};

use golemdb_api::marker::{self, Indexable};
use golemdb_conformance::*;

fn name(s: &str) -> CellName {
    CellNameRef::raw(s.as_bytes()).into()
}

fn eq<T: Indexable>(field: &str, t: T, value: T::Value<'_>) -> Filter {
    Filter::Eq {
        field: name(field),
        value: EqValue::of(t, value),
    }
}

fn in_<'a, T: Indexable>(
    field: &str,
    t: T,
    values: impl IntoIterator<Item = T::Value<'a>>,
) -> Filter {
    Filter::In {
        field: name(field),
        values: InValues::of(t, values),
    }
}

fn range<'a, T: Indexable>(
    field: &str,
    t: T,
    lower: Bound<T::Value<'a>>,
    upper: Bound<T::Value<'a>>,
) -> Filter {
    Filter::Range {
        field: name(field),
        bounds: Bounds::new(t, lower, upper),
    }
}

fn prefix(field: &str, p: &str) -> Filter {
    Filter::Prefix {
        field: name(field),
        prefix: p.to_string(),
    }
}

/// The keys `filter` matches at the head, in result order, as the first byte of
/// each key. Also checks what every query must agree on: `total_matched`,
/// `count`, `evaluated_at`.
#[track_caller]
fn matches(db: &Database, filter: Filter) -> Vec<u8> {
    let head = db.head().unwrap();
    let page = db
        .query(None, QueryOp::from_filter(filter.clone(), B).with_total())
        .ok();
    let keys: Vec<u8> = page.records.iter().map(|r| r.key.0[0]).collect();
    assert_eq!(page.total_matched, Some(keys.len() as u64), "total_matched");
    assert_eq!(page.evaluated_at, head, "evaluated_at");
    let count = db.count(None, CountOp::filter(filter, B)).ok();
    assert_eq!(count, keys.len() as u64, "count");
    keys
}

/// Seven records, created in key order in one commit, so record ID order is key order.
fn dataset() -> Database {
    let db = db();
    let s = CellValue::from_str;
    let records: Vec<RecordOp<op::Create>> = vec![
        RecordOp::create(key(1), B)
            .attribute("color", s("red"))
            .attribute("n", CellValue::from_u32(1))
            .attribute("price", CellValue::from_i64(-5))
            .attribute("label", s("apple")),
        RecordOp::create(key(2), B)
            .attribute("color", s("blue"))
            .attribute("n", CellValue::from_u32(2))
            .attribute("price", CellValue::from_i64(0))
            .attribute("label", s("apricot")),
        RecordOp::create(key(3), B)
            .attribute("color", s("red"))
            .attribute("n", CellValue::from_u32(3))
            .attribute("price", CellValue::from_i64(10))
            .attribute("label", s("banana")),
        // Same names, other types; `n` a field.
        RecordOp::create(key(4), B)
            .attribute("color", CellValue::from_u32(7))
            .field("n", CellValue::from_u32(4))
            .attribute("price", CellValue::from_i32(0))
            .attribute("label", CellValue::from_u32(1)),
        RecordOp::create(key(5), B)
            .attribute("n", CellValue::from_u32(5))
            .attribute("price", CellValue::from_i64(i64::MIN))
            .attribute("label", s("")),
        RecordOp::create(key(6), B)
            .attribute("color", s("Red"))
            .attribute("n", CellValue::from_u64(6))
            .attribute("label", s("ap")),
        // Only fields: never indexed.
        RecordOp::create(key(7), B)
            .field("color", s("red"))
            .field("label", s("apple")),
    ];
    in_commit(&db, |b| {
        for op in records {
            db.create(b, op).ok();
        }
    });
    db
}

/// "Leaves are typed … a cell of another type under the same name is absent";
/// matching is exact (case-sensitive); fields are not indexed.
#[test]
fn eq_matches_exact_typed_attributes() {
    let db = dataset();
    assert_eq!(matches(&db, eq("color", marker::Str, "red")), [1, 3]);
    assert_eq!(matches(&db, eq("color", marker::Str, "Red")), [6]);
    assert_eq!(matches(&db, eq("color", marker::U32, 7)), [4]);
    assert_eq!(matches(&db, eq("color", marker::U64, 7)), [0u8; 0]);
    assert_eq!(matches(&db, eq("color", marker::Str, "green")), [0u8; 0]);
    assert_eq!(matches(&db, eq("nope", marker::Str, "red")), [0u8; 0]);
    // u32 vs u64 under one name: typed.
    assert_eq!(matches(&db, eq("n", marker::U32, 6)), [0u8; 0]);
    assert_eq!(matches(&db, eq("n", marker::U64, 6)), [6]);
    // i32 0 and i64 0 are different values.
    assert_eq!(matches(&db, eq("price", marker::I64, 0)), [2]);
    assert_eq!(matches(&db, eq("price", marker::I32, 0)), [4]);
}

/// Only attributes are indexed ("bit `0x80` is set for an attribute (indexed)"): fields never match.
#[test]
fn fields_never_match() {
    let db = dataset();
    assert_eq!(matches(&db, eq("n", marker::U32, 4)), [0u8; 0]);
    assert_eq!(matches(&db, eq("label", marker::Str, "apple")), [1]); // not 7
    assert_eq!(matches(&db, prefix("label", "app")), [1]);
}

/// "`In`: … one term read each"; no value matches nothing.
#[test]
fn in_matches_any_of_its_values() {
    let db = dataset();
    assert_eq!(
        matches(&db, in_("color", marker::Str, ["red", "blue"])),
        [1, 2, 3]
    );
    assert_eq!(
        matches(&db, in_("color", marker::Str, ["blue", "red", "blue"])),
        [1, 2, 3]
    );
    assert_eq!(matches(&db, in_("color", marker::Str, ["red"])), [1, 3]);
    assert_eq!(matches(&db, in_("color", marker::Str, ["nope"])), [0u8; 0]);
    assert_eq!(
        matches(&db, in_("color", marker::Str, [] as [&str; 0])),
        [0u8; 0]
    );
    assert_eq!(matches(&db, in_("n", marker::U32, [1, 3, 4, 5])), [1, 3, 5]);
}

/// "Each side of a `Range` is an explicit `std::ops::Bound`"
#[test]
fn range_bounds() {
    let db = dataset();
    let p = |lo, hi| matches(&db, range("price", marker::I64, lo, hi));
    assert_eq!(p(Included(-5), Excluded(10)), [1, 2]);
    assert_eq!(p(Excluded(-5), Included(10)), [2, 3]);
    assert_eq!(p(Unbounded, Included(0)), [1, 2, 5]);
    assert_eq!(p(Excluded(0), Unbounded), [3]);
    assert_eq!(p(Included(i64::MIN), Included(i64::MIN)), [5]);
}

/// "`Included(a)` and `Included(a)` | the same as `Eq a`"
#[test]
fn range_with_equal_inclusive_bounds_is_eq() {
    let db = dataset();
    for v in [-5, 0, 10, 3] {
        assert_eq!(
            matches(&db, range("price", marker::I64, Included(v), Included(v))),
            matches(&db, eq("price", marker::I64, v)),
        );
    }
}

/// "`Included(a)` and `Excluded(a)`, or the lower above the upper | **empty**: no error, nothing read"
#[test]
fn ranges_that_admit_no_value_match_nothing() {
    let db = dataset();
    let p = |lo, hi| matches(&db, range("price", marker::I64, lo, hi));
    assert_eq!(p(Included(0), Excluded(0)), [0u8; 0]);
    assert_eq!(p(Excluded(0), Included(0)), [0u8; 0]);
    assert_eq!(p(Excluded(0), Excluded(0)), [0u8; 0]);
    assert_eq!(p(Included(10), Included(-5)), [0u8; 0]);
}

/// "A `Range` keeps its type even with two unbounded sides, which match every
/// cell of that type." (The proposal's bounds table says `InvalidQuery` instead.)
#[test]
fn unbounded_range_matches_every_cell_of_its_type() {
    let db = dataset();
    assert_eq!(
        matches(&db, range("price", marker::I64, Unbounded, Unbounded)),
        [1, 2, 3, 5]
    );
    assert_eq!(
        matches(&db, range("price", marker::I32, Unbounded, Unbounded)),
        [4]
    );
    assert_eq!(
        matches(&db, range("color", marker::Str, Unbounded, Unbounded)),
        [1, 2, 3, 6]
    );
}

/// "`Range` … in that type's order: numeric for numbers and dates, false before
/// true, byte order for strings (UTF-8) and fixed bytes". For each type: one
/// record per value, then every `[a, b)` and `(a, b]` slice must select exactly
/// the values between.
fn ordered<T: Indexable + Copy>(t: T, sorted: &[T::Value<'static>])
where
    T::Value<'static>: Copy + std::fmt::Debug,
{
    let db = db();
    in_commit(&db, |b| {
        for (i, v) in sorted.iter().enumerate() {
            db.create(
                b,
                RecordOp::create(key(i as u8 + 1), B).attribute("v", T::cell(*v)),
            )
            .ok();
        }
    });
    let all: Vec<u8> = (1..=sorted.len() as u8).collect();
    assert_eq!(matches(&db, range("v", t, Unbounded, Unbounded)), all);
    for i in 0..sorted.len() {
        for j in i..sorted.len() {
            let (a, b) = (sorted[i], sorted[j]);
            let half_open = matches(&db, range("v", t, Included(a), Excluded(b)));
            assert_eq!(half_open, all[i..j], "[{a:?}, {b:?})");
            let left_open = matches(&db, range("v", t, Excluded(a), Included(b)));
            assert_eq!(
                left_open,
                all[(i + 1).min(j)..j + usize::from(i < j)].to_vec(),
                "({a:?}, {b:?}]"
            );
        }
    }
}

#[test]
fn range_order_integers() {
    ordered(marker::U32, &[0, 1, 255, 256, u32::MAX]);
    ordered(
        marker::U64,
        &[0, 1, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX],
    );
    ordered(marker::U128, &[0, 1, u64::MAX as u128 + 1, u128::MAX]);
    ordered(marker::I32, &[i32::MIN, -256, -1, 0, 1, 256, i32::MAX]);
    ordered(marker::I64, &[i64::MIN, -1, 0, 1, i64::MAX]);
    ordered(marker::I128, &[i128::MIN, -1, 0, 1, i128::MAX]);
}

#[test]
fn range_order_wide_integers() {
    let be = |hi: u8, lo: u8| {
        let mut v = [0u8; 32];
        v[0] = hi;
        v[31] = lo;
        v
    };
    ordered(
        marker::U256,
        &[be(0, 0), be(0, 1), be(0, 255), be(1, 0), [0xff; 32]],
    );
    // Two's complement, big-endian: MIN, -1, 0, 1, MAX.
    let mut max = [0xff; 32];
    max[0] = 0x7f;
    ordered(
        marker::I256,
        &[be(0x80, 0), [0xff; 32], be(0, 0), be(0, 1), max],
    );
    ordered(marker::Dec256, &[be(0x80, 0), [0xff; 32], be(0, 0), max]);
}

#[test]
fn range_order_decimals_dates_and_timestamps() {
    ordered(marker::Dec32, &[i32::MIN, -1, 0, 1, i32::MAX]);
    ordered(marker::Dec64, &[i64::MIN, -1, 0, i64::MAX]);
    ordered(marker::Dec128, &[i128::MIN, 0, i128::MAX]);
    ordered(
        marker::Date32,
        &[i32::MIN, -719_162, -1, 0, 1, 20_000, i32::MAX],
    );
    ordered(
        marker::Timestamp64,
        &[i64::MIN, -1, 0, 1_700_000_000_000_000, i64::MAX],
    );
}

#[test]
fn range_order_floats() {
    let f = |v: f64| NotNan64::new(v).unwrap();
    ordered(
        marker::F64,
        &[
            f(f64::NEG_INFINITY),
            f(f64::MIN),
            f(-1.5),
            f(-f64::MIN_POSITIVE),
            f(0.0),
            f(f64::MIN_POSITIVE),
            f(1.5),
            f(f64::MAX),
            f(f64::INFINITY),
        ],
    );
    let f = |v: f32| NotNan32::new(v).unwrap();
    ordered(
        marker::F32,
        &[
            f(f32::NEG_INFINITY),
            f(-1.0),
            f(0.0),
            f(1.0),
            f(f32::INFINITY),
        ],
    );
}

#[test]
fn range_order_strings_bytes_and_bool() {
    // Byte order of UTF-8: uppercase before lowercase, a prefix before its extensions.
    ordered(
        marker::Str,
        &["", "A", "B", "a", "ab", "b", "z", "é", "\u{1F600}"],
    );
    ordered(
        marker::Bytes4,
        &[
            [0; 4],
            [0, 0, 0, 1],
            [0, 0, 1, 0],
            [0x80, 0, 0, 0],
            [0xff; 4],
        ],
    );
    ordered(marker::Bytes20, &[[0; 20], [1; 20], [0xff; 20]]);
    ordered(
        marker::Bytes32,
        &[[0; 32], [0x7f; 32], [0x80; 32], [0xff; 32]],
    );
    ordered(marker::Bool, &[false, true]);
}

/// "`Prefix` is `str` only. It matches the strings that start with the prefix,
/// byte by byte in UTF-8." The empty prefix matches every `str` cell.
#[test]
fn prefix_matches_str_attributes_by_leading_bytes() {
    let db = dataset();
    assert_eq!(matches(&db, prefix("label", "ap")), [1, 2, 6]);
    assert_eq!(matches(&db, prefix("label", "apple")), [1]);
    assert_eq!(matches(&db, prefix("label", "apples")), [0u8; 0]);
    assert_eq!(matches(&db, prefix("label", "b")), [3]);
    assert_eq!(matches(&db, prefix("label", "A")), [0u8; 0]);
    assert_eq!(matches(&db, prefix("label", "")), [1, 2, 3, 5, 6]); // not 4 (u32), not 7 (field)
    assert_eq!(matches(&db, prefix("n", "")), [0u8; 0]);
}

#[test]
fn prefix_is_byte_wise_over_multi_byte_characters() {
    let db = db();
    in_commit(&db, |b| {
        for (n, s) in [(1, "é"), (2, "éa"), (3, "e"), (4, "ê")] {
            db.create(
                b,
                RecordOp::create(key(n), B).attribute("s", CellValue::from_str(s)),
            )
            .ok();
        }
    });
    assert_eq!(matches(&db, prefix("s", "é")), [1, 2]);
    assert_eq!(matches(&db, prefix("s", "e")), [3]);
}

/// "`And`: children intersect"; "`Or`: … union"; "A record matching several
/// children of an `Or` is returned **once**."
#[test]
fn and_intersects_and_or_unions() {
    let db = dataset();
    let red = || eq("color", marker::Str, "red");
    let cheap = || range("price", marker::I64, Unbounded, Included(0));
    assert_eq!(matches(&db, Filter::And(vec![red(), cheap()])), [1]);
    assert_eq!(matches(&db, Filter::Or(vec![red(), cheap()])), [1, 2, 3, 5]);
    assert_eq!(matches(&db, Filter::Or(vec![red(), red(), red()])), [1, 3]);
    assert_eq!(matches(&db, Filter::And(vec![red()])), [1, 3]);
    assert_eq!(matches(&db, Filter::Or(vec![red()])), [1, 3]);
    assert_eq!(
        matches(
            &db,
            Filter::And(vec![red(), eq("color", marker::Str, "blue")])
        ),
        [0u8; 0]
    );
}

/// "ordering affects _whether_ you get results, never _which_": child order
/// never changes the result.
#[test]
fn child_order_does_not_change_the_result() {
    let db = dataset();
    let leaves = [
        eq("color", marker::Str, "red"),
        range("price", marker::I64, Included(-5), Unbounded),
        prefix("label", "a"),
        in_("n", marker::U32, [1, 2, 3]),
    ];
    for node in [Filter::And, Filter::Or] {
        let forward = matches(&db, node(leaves.to_vec()));
        let mut reversed = leaves.to_vec();
        reversed.reverse();
        assert_eq!(matches(&db, node(reversed)), forward);
    }
}

/// "`(a ∨ b) ∧ c`": nested trees evaluate as written.
#[test]
fn nested_trees() {
    let db = dataset();
    let tree = Filter::And(vec![
        Filter::Or(vec![
            eq("color", marker::Str, "red"),
            eq("color", marker::Str, "blue"),
        ]),
        Filter::Or(vec![
            prefix("label", "ban"),
            Filter::And(vec![
                range("price", marker::I64, Excluded(-5), Unbounded),
                prefix("label", "apr"),
            ]),
        ]),
    ]);
    assert_eq!(matches(&db, tree), [2, 3]);
}

/// "that `And` and `Or` are not empty" → `InvalidQuery`, for `query` and `count`.
#[test]
fn empty_and_or_are_invalid_query() {
    let db = dataset();
    for filter in [
        Filter::And(vec![]),
        Filter::Or(vec![]),
        Filter::And(vec![eq("color", marker::Str, "red"), Filter::Or(vec![])]),
    ] {
        assert_err!(
            db.query(None, QueryOp::from_filter(filter.clone(), B))
                .result,
            ApiError::InvalidQuery { .. }
        );
        assert_err!(
            db.count(None, CountOp::filter(filter, B)).result,
            ApiError::InvalidQuery { .. }
        );
    }
}

/// "`#maxQueryDepth` | nesting depth of the filter tree | … | code ceiling 64":
/// a tree deeper than any allowed cap is `LimitExceeded`; shallow trees run.
#[test]
fn deep_trees_are_limit_exceeded() {
    let db = dataset();
    let nest = |depth: usize| {
        (0..depth).fold(eq("color", marker::Str, "red"), |f, _| Filter::And(vec![f]))
    };
    assert_eq!(matches(&db, nest(4)), [1, 3]);
    assert_err!(
        db.query(None, QueryOp::from_filter(nest(1000), B)).result,
        ApiError::LimitExceeded { .. }
    );
    assert_err!(
        db.count(None, CountOp::filter(nest(1000), B)).result,
        ApiError::LimitExceeded { .. }
    );
}

/// "Four genesis parameters in `#params`, each `u32` … readable with `get`"
#[test]
#[ignore = "spec-gap: query caps (#maxQueryDepth, #maxQueryLimit, #maxQueryBytes, #maxSortKeys) not in #params"]
fn query_caps_are_in_params() {
    let db = db();
    let params = read(&db, ReadTarget::Head, reserved_key("#params"));
    for cap in [
        "#maxQueryDepth",
        "#maxQueryLimit",
        "#maxQueryBytes",
        "#maxSortKeys",
    ] {
        assert!(
            cell(&params, cap).and_then(CellValue::as_u32).is_some(),
            "{cap}"
        );
    }
}

/// Admission "checks names": an invalid field name is `InvalidArgument`.
#[test]
fn invalid_field_names_are_invalid_argument() {
    let db = dataset();
    for field in ["", "bad name", "1a", "#key"] {
        for filter in [
            eq(field, marker::Str, "x"),
            in_(field, marker::Str, ["x"]),
            range(field, marker::Str, Included("a"), Unbounded),
            prefix(field, "x"),
        ] {
            let result = db
                .query(None, QueryOp::from_filter(filter.clone(), B))
                .result;
            assert!(
                matches!(result, Err(ApiError::InvalidArgument { .. })),
                "{field:?} in {filter:?}: {:?}",
                result.map(drop)
            );
        }
    }
}

/// Admission "checks names" — taken here to include `#maxCellNameLen`, as for record calls.
#[test]
#[ignore = "spec-gap (ambiguous): query admission does not check #maxCellNameLen"]
fn field_names_over_the_length_limit_are_invalid_argument() {
    let db = dataset();
    let long = "abcdefghijklmnopqrstuvwxyzABCDEFG"; // 33 bytes
    assert_err!(
        db.query(None, QueryOp::from_filter(eq(long, marker::Str, "x"), B))
            .result,
        ApiError::InvalidArgument { .. }
    );
}

/// "Queries run against **committed state only**, never against a branch."
#[test]
fn queries_see_committed_state_only() {
    let db = dataset();
    let b = db.begin().unwrap();
    db.create(
        b,
        RecordOp::create(key(9), B).attribute("color", CellValue::from_str("red")),
    )
    .ok();
    db.delete(b, RecordOp::delete(key(1), B)).ok();
    assert_eq!(matches(&db, eq("color", marker::Str, "red")), [1, 3]);
    db.commit(b).unwrap();
    assert_eq!(matches(&db, eq("color", marker::Str, "red")), [3, 9]);
}

/// The index follows every write: patched values move, removed and deleted ones
/// leave, and an attribute turned into a field leaves.
#[test]
fn the_index_follows_patches_and_deletes() {
    let db = dataset();
    in_commit(&db, |b| {
        db.patch(
            b,
            RecordOp::patch(key(1), B).attribute("color", CellValue::from_str("blue")),
        )
        .ok();
        db.patch(
            b,
            RecordOp::patch(key(3), B).field("color", CellValue::from_str("red")),
        )
        .ok();
        db.patch(b, RecordOp::patch(key(6), B).remove("color")).ok();
        db.delete(b, RecordOp::delete(key(2), B)).ok();
    });
    assert_eq!(matches(&db, eq("color", marker::Str, "red")), [0u8; 0]);
    assert_eq!(matches(&db, eq("color", marker::Str, "blue")), [1]);
    assert_eq!(matches(&db, eq("color", marker::Str, "Red")), [0u8; 0]);
    assert_eq!(
        matches(&db, range("color", marker::Str, Unbounded, Unbounded)),
        [1]
    );
    assert_eq!(
        matches(&db, range("price", marker::I64, Unbounded, Unbounded)),
        [1, 3, 5]
    );
}

/// "Without `sort`: record ID ascending, which is creation order" — not key order;
/// a re-created key gets a new ID and moves to the end.
#[test]
fn results_come_in_creation_order() {
    let db = db();
    for n in [5u8, 3, 9, 1] {
        commit_create(
            &db,
            RecordOp::create(key(n), B).attribute("t", CellValue::from_bool(true)),
        );
    }
    let t = || eq("t", marker::Bool, true);
    assert_eq!(matches(&db, t()), [5, 3, 9, 1]);
    in_commit(&db, |b| {
        db.delete(b, RecordOp::delete(key(3), B)).ok();
        db.create(
            b,
            RecordOp::create(key(3), B).attribute("t", CellValue::from_bool(true)),
        )
        .ok();
    });
    assert_eq!(matches(&db, t()), [5, 9, 1, 3]);
}

/// "records are returned as `get` returns them … with projection"
#[test]
fn records_are_returned_as_get_returns_them() {
    let db = dataset();
    let page = db
        .query(
            None,
            QueryOp::from_filter(eq("color", marker::Str, "red"), B),
        )
        .ok();
    for record in &page.records {
        assert_eq!(record, &read(&db, ReadTarget::Head, record.key));
    }
    let projected = db
        .query(
            None,
            QueryOp::from_filter(eq("color", marker::Str, "red"), B).only(["label", "nope"]),
        )
        .ok();
    for record in &projected.records {
        let expected = db
            .get(
                ReadTarget::Head,
                RecordOp::get(record.key, B).only(["label", "nope"]),
            )
            .ok();
        assert_eq!(record, &expected);
        assert_eq!(record.cells.len(), 1);
    }
}

/// "`total_matched`" is reported when asked for.
#[test]
fn total_matched_only_when_asked() {
    let db = dataset();
    let filter = eq("color", marker::Str, "red");
    let page = db.query(None, QueryOp::from_filter(filter.clone(), B)).ok();
    assert_eq!(page.total_matched, None);
    let page = db
        .query(None, QueryOp::from_filter(filter, B).with_total())
        .ok();
    assert_eq!(page.total_matched, Some(2));
}

/// "`commit` is `None` for the head … or `Some(c)` … For now only the head can
/// be read; a commit other than the head returns `CommitUnavailable`"
#[test]
fn only_the_head_can_be_queried() {
    let db = dataset();
    let head = db.head().unwrap();
    let filter = || eq("color", marker::Str, "red");
    let page = db.query(Some(head), QueryOp::from_filter(filter(), B)).ok();
    assert_eq!(page.records.len(), 2);
    assert_eq!(db.count(Some(head), CountOp::filter(filter(), B)).ok(), 2);
    for requested in [CommitId::GENESIS, CommitId::new(head.get() + 1)] {
        assert_err!(
            db.query(Some(requested), QueryOp::from_filter(filter(), B)).result,
            ApiError::CommitUnavailable { requested: r, head: h } if r == requested && h == head
        );
        assert_err!(
            db.count(Some(requested), CountOp::filter(filter(), B))
                .result,
            ApiError::CommitUnavailable { .. }
        );
    }
}

fn not_implemented_ops() -> [QueryOp; 3] {
    let f = || eq("color", marker::Str, "red");
    [
        QueryOp::from_filter(f(), B).sort([SortKey::asc("n", CellType::Uint(Width::W4))]),
        QueryOp::from_filter(f(), B).limit(1),
        QueryOp::from_filter(f(), B).offset(1),
    ]
}

/// Staging: "unsupported parts return `NotImplemented`, naming the part"
#[test]
fn sort_limit_and_offset_are_not_implemented_yet() {
    let db = dataset();
    for op in not_implemented_ops() {
        assert_err!(db.query(None, op).result, ApiError::NotImplemented { .. });
    }
}

/// QR5 "Order of checks: first the top-level parts not implemented yet, at no
/// cost; then admission …; then one storage snapshot is opened and the requested
/// commit is checked against its head"
#[test]
#[ignore = "spec-gap: the commit is checked before NotImplemented parts and admission (query proposal QR5)"]
fn not_implemented_parts_are_checked_first() {
    let db = dataset();
    for op in not_implemented_ops() {
        assert_err!(
            db.query(Some(CommitId::new(99)), op).result,
            ApiError::NotImplemented { .. }
        );
    }
}

#[test]
#[ignore = "spec-gap: the commit is checked before NotImplemented parts and admission (query proposal QR5)"]
fn admission_comes_before_the_commit_check() {
    let db = dataset();
    assert_err!(
        db.query(
            Some(CommitId::new(99)),
            QueryOp::from_filter(Filter::Or(vec![]), B)
        )
        .result,
        ApiError::InvalidQuery { .. }
    );
    assert_err!(
        db.count(
            Some(CommitId::new(99)),
            CountOp::filter(Filter::Or(vec![]), B)
        )
        .result,
        ApiError::InvalidQuery { .. }
    );
}

/// "The same receipt as the record calls … `priced_at` (the head at admission)
/// and `details`, which are zero for reads."
#[test]
fn query_receipts() {
    let db = dataset();
    let head = Some(db.head().unwrap());
    let filter = || eq("color", marker::Str, "red");
    let q = db.query(None, QueryOp::from_filter(filter(), B));
    let c = db.count(None, CountOp::filter(filter(), B));
    let failed = db.query(Some(CommitId::GENESIS), QueryOp::from_filter(filter(), B));
    for receipt in [&q.receipt, &c.receipt, &failed.receipt] {
        assert_eq!(receipt.priced_at, head);
        assert_eq!(receipt.cost, 0);
        assert_eq!(receipt.details, Details::default());
    }
}
