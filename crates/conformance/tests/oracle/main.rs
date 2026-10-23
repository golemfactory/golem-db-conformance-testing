//! Random operation sequences, run against golem-db and the SQLite
//! oracle (`mirror`) side by side. After every operation
//! its result, error variant and receipt must match, and so must the full state
//! of the head and of every open branch.
//!
//! `PROPTEST_CASES` sets the number of sequences (default 64). A failure is
//! shrunk to a minimal sequence and printed.

use std::ops::Bound;

mod mirror;
mod value;

use golemdb_conformance::*;
use mirror::{Change, Expected, Kind, Limits, Mirror, Qf};
use proptest::prelude::*;
use proptest::sample::select;
use proptest::test_runner::TestCaseError;
use value::{Tv, Ty};

macro_rules! check {
    ($cond:expr, $($fmt:tt)*) => {
        if !$cond {
            return Err(TestCaseError::fail(format!($($fmt)*)));
        }
    };
}

/// Small caps, so that the name pool can exceed them.
const CAPS: RecordLimits = RecordLimits {
    max_cells: 4,
    max_indexed_cells: 3,
};
const KEYS: u8 = 6;
/// Key indices `KEYS..2 * KEYS` pick a key the target already holds, if any.
const KEY_CHOICES: u8 = 2 * KEYS;

// ---- generators --------------------------------------------------------

fn valid_name() -> impl Strategy<Value = String> {
    select(vec!["a", "b", "c", "d", "$e"]).prop_map(String::from)
}

/// Mostly valid names; rarely one breaking the grammar or the length limit.
fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        40 => valid_name(),
        1 => Just("bad name".to_string()),
        1 => Just("abcdefghijklmnopqrstuvwxyzABCDEFG".to_string()), // 33 bytes
    ]
}

fn query_name() -> impl Strategy<Value = String> {
    prop_oneof![
        60 => select(vec!["a", "b", "c", "d", "$e", "nope"]).prop_map(String::from),
        1 => Just("bad name".to_string()),
    ]
}

fn value_of(ty: Ty) -> BoxedStrategy<Tv> {
    match ty {
        Ty::Bool => any::<bool>().prop_map(Tv::Bool).boxed(),
        Ty::Str => prop_oneof![
            30 => select(vec!["", "a", "ab", "abc", "b", "Z", "é", "éa"]).prop_map(|s| Tv::Str(s.into())),
            1 => Just(Tv::Str("x".repeat(65))),
        ]
        .boxed(),
        Ty::Bytes => prop_oneof![
            30 => proptest::collection::vec(any::<u8>(), 0..4).prop_map(Tv::Bytes),
            1 => Just(Tv::Bytes(vec![7; 129])),
        ]
        .boxed(),
        Ty::Bytes4 => select(vec![[0; 4], [0, 0, 0, 1], [0x80, 0, 0, 0], [0xff; 4]]).prop_map(Tv::Bytes4).boxed(),
        Ty::U32 => select(vec![0, 1, 1, 2, 2, u32::MAX]).prop_map(Tv::U32).boxed(),
        Ty::U64 => select(vec![0, 1, 2, i64::MAX as u64 + 1, u64::MAX]).prop_map(Tv::U64).boxed(),
        Ty::U128 => select(vec![0, 1, u128::MAX]).prop_map(Tv::U128).boxed(),
        Ty::I32 => select(vec![i32::MIN, -1, 0, 1, i32::MAX]).prop_map(Tv::I32).boxed(),
        Ty::I64 => select(vec![i64::MIN, -2, -1, 0, 1, 2, i64::MAX]).prop_map(Tv::I64).boxed(),
        Ty::I128 => select(vec![i128::MIN, -1, 0, 1, i128::MAX]).prop_map(Tv::I128).boxed(),
        Ty::Dec64 => select(vec![i64::MIN, -1, 0, 1, i64::MAX]).prop_map(Tv::Dec64).boxed(),
        Ty::F32 => select(vec![f32::NEG_INFINITY, -1.5, -0.0, 0.0, 1.5, f32::INFINITY]).prop_map(Tv::F32).boxed(),
        Ty::F64 => select(vec![f64::NEG_INFINITY, -1.5, -0.0, 0.0, f64::MIN_POSITIVE, 1.5, f64::INFINITY])
            .prop_map(Tv::F64)
            .boxed(),
        Ty::Date32 => select(vec![i32::MIN, -1, 0, 1, i32::MAX]).prop_map(Tv::Date32).boxed(),
        Ty::Timestamp64 => select(vec![i64::MIN, 0, 1, i64::MAX]).prop_map(Tv::Timestamp64).boxed(),
    }
}

/// Few types, so that one name often holds several types across records.
fn indexable_ty() -> impl Strategy<Value = Ty> {
    prop_oneof![
        8 => select(vec![Ty::U32, Ty::Str, Ty::I64]),
        1 => select(Ty::ALL.into_iter().filter(|t| *t != Ty::Bytes).collect::<Vec<_>>()),
    ]
}

fn indexable_value() -> impl Strategy<Value = Tv> {
    indexable_ty().prop_flat_map(value_of)
}

fn any_value() -> impl Strategy<Value = Tv> {
    prop_oneof![10 => indexable_value(), 1 => value_of(Ty::Bytes)]
}

fn kind() -> impl Strategy<Value = Kind> {
    select(vec![Kind::Field, Kind::Attribute])
}

/// A value and the kind to write it with. Bytes are mostly fields: as
/// attributes they are a (tested) admission error.
fn write_value() -> impl Strategy<Value = (Tv, Kind)> {
    (any_value(), kind(), 0..10u8).prop_map(|(v, k, roll)| {
        let k = if v.ty() == Ty::Bytes && roll > 0 {
            Kind::Field
        } else {
            k
        };
        (v, k)
    })
}

/// A create's cells: mostly distinct names; sometimes a name twice (an
/// admission error). Up to five cells, one over `CAPS.max_cells`.
fn create_cells() -> impl Strategy<Value = Vec<(String, Tv, Kind)>> {
    prop_oneof![
        9 => proptest::collection::btree_map(valid_name(), write_value(), 0..=5)
            .prop_map(|m| m.into_iter().map(|(n, (v, k))| (n, v, k)).collect()),
        1 => proptest::collection::vec((name(), write_value()).prop_map(|(n, (v, k))| (n, v, k)), 0..=5),
    ]
}

fn change_of(name: String) -> impl Strategy<Value = (String, Change)> {
    prop_oneof![
        4 => write_value().prop_map(|(v, k)| Change::Write(v, k)),
        // Any value: `set` of bytes onto an attribute is a tested error.
        2 => any_value().prop_map(Change::Set),
        2 => Just(Change::Remove),
    ]
    .prop_map(move |c| (name.clone(), c))
}

/// A patch's changes: mostly distinct names, like `create_cells`.
fn changes() -> impl Strategy<Value = Vec<(String, Change)>> {
    prop_oneof![
        9 => proptest::collection::btree_set(valid_name(), 0..4).prop_flat_map(|names| {
            names.into_iter().map(change_of).collect::<Vec<_>>()
        }),
        1 => proptest::collection::vec(name().prop_flat_map(change_of), 0..4),
    ]
}

fn bound(ty: Ty) -> impl Strategy<Value = Bound<Tv>> {
    prop_oneof![
        2 => value_of(ty).prop_map(Bound::Included),
        2 => value_of(ty).prop_map(Bound::Excluded),
        1 => Just(Bound::Unbounded),
    ]
}

fn leaf() -> impl Strategy<Value = Qf> {
    prop_oneof![
        3 => (query_name(), indexable_value()).prop_map(|(n, v)| Qf::Eq(n, v)),
        2 => (query_name(), indexable_ty())
            .prop_flat_map(|(n, ty)| (Just(n), Just(ty), proptest::collection::vec(value_of(ty), 0..4)))
            .prop_map(|(n, ty, vs)| Qf::In(n, ty, vs)),
        3 => (query_name(), indexable_ty())
            .prop_flat_map(|(n, ty)| (Just(n), Just(ty), bound(ty), bound(ty)))
            .prop_map(|(n, ty, lo, hi)| Qf::Range(n, ty, lo, hi)),
        2 => (query_name(), select(vec!["", "a", "ab", "é", "Z"])).prop_map(|(n, p)| Qf::Prefix(n, p.into())),
        2 => (query_name(), proptest::option::of(indexable_ty())).prop_map(|(n, ty)| Qf::Exists(n, ty)),
    ]
}

fn filter() -> impl Strategy<Value = Qf> {
    leaf().prop_recursive(3, 12, 3, |inner| {
        prop_oneof![
            15 => proptest::collection::vec(inner.clone(), 1..4).prop_map(Qf::And),
            15 => proptest::collection::vec(inner, 1..4).prop_map(Qf::Or),
            1 => prop_oneof![Just(Qf::And(vec![])), Just(Qf::Or(vec![]))],
        ]
    })
}

#[derive(Debug, Clone)]
enum Op {
    Begin,
    Create {
        b: u8,
        k: u8,
        cells: Vec<(String, Tv, Kind)>,
    },
    Patch {
        b: u8,
        k: u8,
        changes: Vec<(String, Change)>,
    },
    Delete {
        b: u8,
        k: u8,
    },
    GetBranch {
        b: u8,
        k: u8,
        only: Option<Vec<String>>,
    },
    GetHead {
        k: u8,
        only: Option<Vec<String>>,
    },
    Checkpoint {
        b: u8,
    },
    Rollback {
        b: u8,
    },
    Seal {
        b: u8,
    },
    Commit {
        b: u8,
    },
    Discard {
        b: u8,
    },
    /// `anchor` seeds which leaves are rewritten to match committed data.
    Query {
        filter: Qf,
        anchor: u8,
        offset: u64,
        limit: Option<u64>,
    },
}

fn op() -> impl Strategy<Value = Op> {
    let b = any::<u8>;
    let k = || 0..KEY_CHOICES;
    let only = || proptest::option::weighted(0.3, proptest::collection::vec(name(), 0..3));
    prop_oneof![
        3 => Just(Op::Begin),
        6 => (b(), k(), create_cells()).prop_map(|(b, k, cells)| Op::Create { b, k, cells }),
        6 => (b(), k(), changes()).prop_map(|(b, k, changes)| Op::Patch { b, k, changes }),
        2 => (b(), k()).prop_map(|(b, k)| Op::Delete { b, k }),
        2 => (b(), k(), only()).prop_map(|(b, k, only)| Op::GetBranch { b, k, only }),
        1 => (k(), only()).prop_map(|(k, only)| Op::GetHead { k, only }),
        2 => b().prop_map(|b| Op::Checkpoint { b }),
        2 => b().prop_map(|b| Op::Rollback { b }),
        1 => b().prop_map(|b| Op::Seal { b }),
        3 => b().prop_map(|b| Op::Commit { b }),
        1 => b().prop_map(|b| Op::Discard { b }),
        4 => (filter(), any::<u8>(), select(vec![0u64, 0, 1, 2, 5]), proptest::option::of(select(vec![0u64, 1, 2, 2, 3, 3, 4, 4, 5, 5])))
            .prop_map(|(filter, anchor, offset, limit)| Op::Query { filter, anchor, offset, limit }),
    ]
}

// ---- conversions -------------------------------------------------------

fn rkey(k: u8) -> RecordKey {
    key(k + 1)
}

/// Rewrites most leaves to use a committed attribute (its name, type and value),
/// so that queries over a few random records still match something.
fn anchor(f: &Qf, attrs: &[(String, Tv)], seed: &mut u32) -> Qf {
    let pick = |seed: &mut u32| {
        *seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (*seed >> 16) as usize
    };
    let leaf = !matches!(f, Qf::And(_) | Qf::Or(_));
    if leaf && (attrs.is_empty() || pick(seed) % 4 == 0) {
        return f.clone();
    }
    let (n, v) = if leaf {
        attrs[pick(seed) % attrs.len()].clone()
    } else {
        (String::new(), Tv::Bool(false))
    };
    match f {
        Qf::Eq(..) => Qf::Eq(n, v),
        Qf::In(_, ty, vs) if *ty == v.ty() => Qf::In(n, *ty, [vs.clone(), vec![v]].concat()),
        Qf::In(..) => Qf::In(n, v.ty(), vec![v]),
        Qf::Range(_, ty, lo, hi) if *ty == v.ty() => Qf::Range(n, *ty, lo.clone(), hi.clone()),
        Qf::Range(..) => match pick(seed) % 4 {
            0 => Qf::Range(n, v.ty(), Bound::Included(v), Bound::Unbounded),
            1 => Qf::Range(n, v.ty(), Bound::Unbounded, Bound::Included(v)),
            2 => Qf::Range(n, v.ty(), Bound::Excluded(v), Bound::Unbounded),
            _ => Qf::Range(n, v.ty(), Bound::Included(v.clone()), Bound::Included(v)),
        },
        Qf::Prefix(..) => match &v {
            Tv::Str(s) => {
                let cut = s
                    .char_indices()
                    .map(|(i, _)| i)
                    .chain([s.len()])
                    .nth(pick(seed) % (s.chars().count() + 1))
                    .unwrap();
                Qf::Prefix(n, s[..cut].to_string())
            }
            _ => Qf::Prefix(n, String::new()),
        },
        Qf::Exists(_, ty) => Qf::Exists(n, ty.map(|_| v.ty())),
        Qf::And(cs) => Qf::And(cs.iter().map(|c| anchor(c, attrs, seed)).collect()),
        Qf::Or(cs) => Qf::Or(cs.iter().map(|c| anchor(c, attrs, seed)).collect()),
    }
}

fn cell_value(c: &mirror::Cell) -> CellValue {
    match c.kind {
        Kind::Attribute => attr(c.value.cell()),
        Kind::Field => c.value.cell(),
    }
}

fn expected_cells(cells: &mirror::Cells) -> std::collections::BTreeMap<String, CellValue> {
    cells
        .iter()
        .map(|(n, c)| (n.clone(), cell_value(c)))
        .collect()
}

fn cell_name(n: &str) -> CellName {
    CellNameRef::raw(n.as_bytes()).into()
}

fn to_filter(f: &Qf) -> Filter {
    let cell = |v: &Tv| v.cell();
    match f {
        Qf::Eq(n, v) => Filter::Eq {
            field: cell_name(n),
            value: EqValue::try_from(v.cell()).unwrap(),
        },
        Qf::In(n, _, vs) => Filter::In {
            field: cell_name(n),
            values: InValues::try_from(vs.iter().map(cell).collect::<Vec<_>>()).unwrap(),
        },
        Qf::Range(n, ty, lo, hi) => Filter::Range {
            field: cell_name(n),
            bounds: Bounds::try_new(ty.cell_type(), lo.as_ref().map(cell), hi.as_ref().map(cell))
                .unwrap(),
        },
        Qf::Prefix(n, p) => Filter::Prefix {
            field: cell_name(n),
            prefix: p.clone(),
        },
        Qf::Exists(n, ty) => Filter::Exists {
            field: cell_name(n),
            ty: ty.map(|t| ExistsType::try_from(t.cell_type()).unwrap()),
        },
        Qf::And(cs) => Filter::And(cs.iter().map(to_filter).collect()),
        Qf::Or(cs) => Filter::Or(cs.iter().map(to_filter).collect()),
    }
}

fn details(d: &Details) -> mirror::Effects {
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

/// Whether golem-db's error is the one the spec prescribes.
fn is(actual: &ApiError, expected: &Expected) -> bool {
    match expected {
        Expected::HandleInvalid => matches!(actual, ApiError::HandleInvalid),
        Expected::Sealed => matches!(actual, ApiError::Sealed),
        Expected::InvalidArgument => matches!(actual, ApiError::InvalidArgument { .. }),
        Expected::AlreadyExists => matches!(actual, ApiError::AlreadyExists),
        Expected::NotFound => matches!(actual, ApiError::NotFound),
        Expected::CellNotFound(n) => {
            matches!(actual, ApiError::CellNotFound { name } if name.as_bytes() == n.as_bytes())
        }
        Expected::NoFrameToRollback => matches!(actual, ApiError::NoFrameToRollback),
        Expected::InvalidQuery => matches!(actual, ApiError::InvalidQuery { .. }),
        // Known spec gap: `main` returns `Conflict` (spec test
        // `branches::commit_on_a_stale_branch_is_handle_invalid`).
        Expected::CommitLost => matches!(actual, ApiError::HandleInvalid | ApiError::Conflict),
    }
}

/// Receipts: "**`cost`** is always 0 until metering is implemented"; `priced_at`
/// is checked on success only, as `priced_at` (`Some(commit)`).
fn receipt(r: &Receipt, priced_at: Option<CommitId>) -> std::result::Result<(), TestCaseError> {
    check!(r.cost == 0, "receipt cost {}", r.cost);
    if let Some(at) = priced_at {
        check!(
            r.priced_at == Some(at),
            "priced_at {:?}, expected {at:?}",
            r.priced_at
        );
    }
    Ok(())
}

/// A read's receipt: "details … are zero for reads"; on success priced at
/// `at` (the head for `Head`, query and count; the origin for a branch).
fn read_receipt<T>(read: &Metered<T>, at: CommitId) -> std::result::Result<(), TestCaseError> {
    let got = details(&read.receipt.details);
    check!(got == [0; 9], "read receipt details {got:?}");
    receipt(&read.receipt, read.result.is_ok().then_some(at))
}

// ---- the runner ----------------------------------------------------------

struct Run {
    db: TestDb,
    mirror: Mirror,
    handles: Vec<BranchId>,
}

/// Compares an outcome: on success the values must agree (`same`), on failure
/// the error must be the expected one.
fn outcome<T: std::fmt::Debug, U: std::fmt::Debug>(
    actual: &std::result::Result<T, ApiError>,
    expected: &std::result::Result<U, Expected>,
    same: impl Fn(&T, &U) -> bool,
) -> std::result::Result<(), TestCaseError> {
    match (actual, expected) {
        (Ok(a), Ok(e)) => check!(same(a, e), "got {a:?}, expected {e:?}"),
        (Err(a), Err(e)) => check!(is(a, e), "got error {a:?}, expected {e:?}"),
        _ => check!(false, "got {actual:?}, expected {expected:?}"),
    }
    Ok(())
}

impl Run {
    fn new() -> Self {
        Self {
            db: open_db(&Genesis::new(HashAlgorithm::Keccak256, LIMITS, CAPS, None)),
            mirror: Mirror::new(Limits {
                max_cell_name_len: LIMITS.max_cell_name_len as usize,
                max_str_len: LIMITS.max_str_len as usize,
                max_bytes_len: LIMITS.max_bytes_len as usize,
                max_cells: CAPS.max_cells,
                max_indexed_cells: CAPS.max_indexed_cells,
            }),
            handles: Vec::new(),
        }
    }

    /// Mostly a branch the oracle holds open, beginning one if there is none;
    /// one call in eight goes to one of the three most recent handles, live or
    /// not, to exercise dead handles.
    fn branch(&mut self, b: u8) -> Option<usize> {
        let n = self.handles.len();
        if !b.is_multiple_of(8) {
            let open: Vec<usize> = (0..n)
                .filter(|&i| self.mirror.peek_next_id(i).is_some())
                .collect();
            if open.is_empty() {
                self.handles.push(self.db.begin().unwrap());
                return Some(self.mirror.begin());
            }
            return Some(open[(b / 8) as usize % open.len()]);
        }
        (n > 0).then(|| n - 1 - (b / 8) as usize % n.min(3))
    }

    /// The record key for index `k`: a literal key, or one the target holds.
    fn key(&self, k: u8, target: Option<usize>) -> RecordKey {
        let held = self.mirror.keys(target);
        if k < KEYS || held.is_empty() {
            return rkey(k % KEYS);
        }
        RecordKey(
            held[(k - KEYS) as usize % held.len()]
                .clone()
                .try_into()
                .unwrap(),
        )
    }

    /// A write's outcome and receipt: details as the effects, cost 0, and on
    /// success priced at the branch's origin commit.
    fn write(
        &mut self,
        branch: usize,
        actual: Metered<()>,
        expected: std::result::Result<mirror::Effects, Expected>,
    ) -> std::result::Result<(), TestCaseError> {
        outcome(&actual.result, &expected, |_, _| true)?;
        let want = expected.as_ref().copied().unwrap_or([0; 9]);
        let got = details(&actual.receipt.details);
        check!(got == want, "receipt details {got:?}, expected {want:?}");
        let origin = expected
            .is_ok()
            .then(|| CommitId::new(self.mirror.origin(branch)));
        receipt(&actual.receipt, origin)
    }

    fn step(&mut self, op: &Op) -> std::result::Result<(), TestCaseError> {
        // A clone shares the database; it frees `self` for `branch`.
        let db: Database = self.db.clone();
        let only_op = |k: RecordKey, only: &Option<Vec<String>>| {
            let op = RecordOp::get(k, B);
            match only {
                Some(names) => op.only(names.iter().map(|n| n.as_bytes())),
                None => op,
            }
        };
        let project = |cells: mirror::Cells, only: &Option<Vec<String>>| match only {
            Some(names) => cells
                .into_iter()
                .filter(|(n, _)| names.contains(n))
                .collect(),
            None => cells,
        };
        match op {
            Op::Begin => {
                self.handles.push(db.begin().unwrap());
                self.mirror.begin();
            }
            Op::Create { b, k, cells } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                let k = self.key(*k, Some(i));
                let mut create = RecordOp::create(k, B);
                for (n, v, kind) in cells {
                    create = match kind {
                        Kind::Attribute => create.attribute(n, v.cell()),
                        Kind::Field => create.field(n, v.cell()),
                    };
                }
                let actual = db.create(self.handles[i], create).map(drop);
                let expected = self.mirror.create(i, &k.0, cells);
                self.write(i, actual, expected)?;
            }
            Op::Patch { b, k, changes } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                let k = self.key(*k, Some(i));
                let mut patch = RecordOp::patch(k, B);
                for (n, change) in changes {
                    patch = match change {
                        Change::Write(v, Kind::Attribute) => patch.attribute(n, v.cell()),
                        Change::Write(v, Kind::Field) => patch.field(n, v.cell()),
                        Change::Set(v) => patch.set(n, v.cell()),
                        Change::Remove => patch.remove(n),
                    };
                }
                let actual = db.patch(self.handles[i], patch);
                let expected = self.mirror.patch(i, &k.0, changes);
                self.write(i, actual, expected)?;
            }
            Op::Delete { b, k } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                let k = self.key(*k, Some(i));
                let actual = db.delete(self.handles[i], RecordOp::delete(k, B));
                let expected = self.mirror.delete(i, &k.0);
                self.write(i, actual, expected)?;
            }
            Op::GetBranch { b, k, only } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                let k = self.key(*k, Some(i));
                let origin = CommitId::new(self.mirror.origin(i));
                let actual = db.get(ReadTarget::Branch(self.handles[i]), only_op(k, only));
                let expected = self.mirror.get_branch(i, &k.0).map(|c| project(c, only));
                outcome(&actual.result, &expected, |r, c| {
                    user_cells(r) == expected_cells(c)
                })?;
                read_receipt(&actual, origin)?;
            }
            Op::GetHead { k, only } => {
                let k = self.key(*k, None);
                let actual = db.get(ReadTarget::Head, only_op(k, only));
                let expected = self
                    .mirror
                    .head_record(&k.0)
                    .ok_or(Expected::NotFound)
                    .map(|c| project(c, only));
                outcome(&actual.result, &expected, |r, c| {
                    user_cells(r) == expected_cells(c)
                })?;
                read_receipt(&actual, CommitId::new(self.mirror.head_commit))?;
            }
            Op::Checkpoint { b } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                outcome(
                    &db.checkpoint(self.handles[i]),
                    &self.mirror.checkpoint(i),
                    |_, _| true,
                )?;
            }
            Op::Rollback { b } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                outcome(
                    &db.rollback(self.handles[i]),
                    &self.mirror.rollback(i),
                    |_, _| true,
                )?;
            }
            Op::Seal { b } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                outcome(&db.seal(self.handles[i]), &self.mirror.seal(i), |_, _| true)?;
            }
            Op::Commit { b } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                outcome(
                    &db.commit(self.handles[i]),
                    &self.mirror.commit(i),
                    |a, e| a.get() == *e,
                )?;
            }
            Op::Discard { b } => {
                let Some(i) = self.branch(*b) else {
                    return Ok(());
                };
                outcome(
                    &db.discard(self.handles[i]),
                    &self.mirror.discard(i),
                    |_, _| true,
                )?;
            }
            Op::Query {
                filter,
                anchor: seed,
                offset,
                limit,
            } => {
                let filter = &anchor(filter, &self.mirror.head_attributes(), &mut (*seed as u32));
                let mut query = QueryOp::from_filter(to_filter(filter), B)
                    .with_total()
                    .offset(*offset);
                if let Some(limit) = limit {
                    query = query.limit(*limit);
                }
                let head = CommitId::new(self.mirror.head_commit);
                let count = db.count(None, CountOp::filter(to_filter(filter), B));
                read_receipt(&count, head)?;
                match (count.result, self.mirror.count(filter)) {
                    (Ok(n), Ok(total)) => check!(n == total, "count {n}, expected {total}"),
                    (Err(e), Err(expected)) => check!(
                        expected.iter().any(|x| is(&e, x)),
                        "count error {e:?}, expected one of {expected:?}"
                    ),
                    (actual, expected) => check!(false, "count {actual:?}, expected {expected:?}"),
                }
                let actual = db.query(None, query);
                read_receipt(&actual, head)?;
                match (actual.result, self.mirror.query(filter, *offset, *limit)) {
                    (Ok(page), Ok((keys, total))) => {
                        let got: Vec<_> = page.records.iter().map(|r| r.key.0.to_vec()).collect();
                        check!(got == keys, "query keys {got:?}, expected {keys:?}");
                        check!(
                            page.total_matched == Some(total),
                            "total_matched {:?}, expected {total}",
                            page.total_matched
                        );
                        for record in &page.records {
                            let expected = self.mirror.head_record(&record.key.0).unwrap();
                            check!(
                                user_cells(record) == expected_cells(&expected),
                                "query record {record:?}"
                            );
                        }
                    }
                    (Err(e), Err(expected)) => {
                        check!(
                            expected.iter().any(|x| is(&e, x)),
                            "query error {e:?}, expected one of {expected:?}"
                        )
                    }
                    (actual, expected) => check!(
                        false,
                        "query {:?}, expected {expected:?}",
                        actual.map(|p| p.records.len())
                    ),
                }
            }
        }
        Ok(())
    }

    /// The whole state: the head, and every branch the oracle holds open.
    fn compare_state(&self) -> std::result::Result<(), TestCaseError> {
        let db = &self.db;
        check!(
            self.handles.len() == self.mirror.branch_count(),
            "{} handles, {} mirrored branches",
            self.handles.len(),
            self.mirror.branch_count()
        );
        check!(
            db.head().unwrap().get() == self.mirror.head_commit,
            "head {:?}",
            db.head()
        );
        let next_id = |target| {
            let alloc = db
                .get(target, RecordOp::get(reserved_key("#alloc"), B))
                .ok();
            cell(&alloc, "#nextRecordID").unwrap().as_u64().unwrap()
        };
        let mut targets = vec![(ReadTarget::Head, None)];
        for (i, handle) in self.handles.iter().enumerate() {
            if self.mirror.peek_next_id(i).is_some() {
                targets.push((ReadTarget::Branch(*handle), Some(i)));
            }
        }
        for (target, branch) in targets {
            let want_id = match branch {
                None => self.mirror.head_next_id(),
                Some(i) => self.mirror.peek_next_id(i).unwrap(),
            };
            check!(
                next_id(target) == want_id,
                "{target:?}: #nextRecordID {}, expected {want_id}",
                next_id(target)
            );
            for k in 0..KEYS {
                let expected = match branch {
                    None => self.mirror.head_record(&rkey(k).0),
                    Some(i) => self.mirror.peek_record(i, &rkey(k).0).unwrap(),
                };
                let actual = db.get(target, RecordOp::get(rkey(k), B)).result;
                match (actual, expected) {
                    (Err(ApiError::NotFound), None) => {}
                    (Ok(record), Some(cells)) => {
                        check!(
                            user_cells(&record) == expected_cells(&cells),
                            "{target:?} key {k}: {record:?}, expected {cells:?}"
                        );
                        check!(
                            cell(&record, "#key") == Some(&CellValue::from_bytes32(rkey(k).0)),
                            "{target:?} key {k}: #key"
                        );
                        check!(
                            meta(&record) == Some(mirror::meta(&cells)),
                            "{target:?} key {k}: #meta {:?}, expected {:?}",
                            meta(&record),
                            mirror::meta(&cells)
                        );
                    }
                    (actual, expected) => check!(
                        false,
                        "{target:?} key {k}: {:?}, expected {expected:?}",
                        actual.map(|r| r.cells)
                    ),
                }
            }
        }
        Ok(())
    }
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    /// `setup` records are created and committed first (through the same
    /// compared steps), so that queries have committed data to match.
    #[test]
    fn golemdb_agrees_with_sqlite(
        setup in proptest::collection::vec(create_cells(), 2..=KEYS as usize),
        // Creation order differs from key order, as result order must follow the former.
        order in Just((0..KEYS).collect::<Vec<u8>>()).prop_shuffle(),
        ops in proptest::collection::vec(op(), 1..60),
    ) {
        let mut run = Run::new();
        let setup = std::iter::once(Op::Begin)
            .chain(setup.into_iter().zip(order).map(|(cells, k)| Op::Create { b: 1, k, cells }))
            .chain([Op::Commit { b: 1 }]);
        let ops: Vec<Op> = setup.chain(ops).collect();
        for (i, op) in ops.iter().enumerate() {
            run.step(op).map_err(|e| TestCaseError::fail(format!("step {i} {op:?}: {e}")))?;
            run.compare_state().map_err(|e| TestCaseError::fail(format!("after step {i} {op:?}: {e}")))?;
        }
    }
}
