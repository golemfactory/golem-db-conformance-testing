//! Shared setup for the spec conformance tests in `tests/spec`.
//!
//! Everything here goes through the public `Api` only: a test writes with
//! `create` / `patch` / `delete` and checks by reading back with `get`.

use std::collections::BTreeMap;

pub use golemdb_api::*;

/// The limits of the spec's genesis example (and of `Genesis::DEV`).
pub const LIMITS: CellLimits = CellLimits {
    max_cell_name_len: 32,
    max_str_len: 64,
    max_bytes_len: 128,
};

/// Per-record caps, roomy enough for every test record (the widest has 24 user
/// cells, 23 of them attributes); `record_caps.rs` tests the caps themselves.
pub const RECORD_LIMITS: RecordLimits = RecordLimits {
    max_cells: 32,
    max_indexed_cells: 24,
};

/// Budgets are not enforced yet; every call states `Unlimited`.
pub const B: Budget = Budget::Unlimited;

pub fn genesis() -> Genesis {
    Genesis::new(HashAlgorithm::Keccak256, LIMITS, RECORD_LIMITS, None)
}

/// A fresh database on MDBX, the store hosts run on, in its own temporary
/// directory. Derefs to `Database`; the directory is removed after the database
/// is dropped.
pub struct TestDb {
    db: Database,
    _dir: tempfile::TempDir,
}

impl std::ops::Deref for TestDb {
    type Target = Database;
    fn deref(&self) -> &Database {
        &self.db
    }
}

pub fn open_db(genesis: &Genesis) -> TestDb {
    let dir = tempfile::tempdir().expect("temporary directory");
    let db = Database::open(
        dir.path(),
        &OpenConfig::new(genesis.clone()).with_mode(OpenMode::CreateNew),
    )
    .expect("open MDBX database");
    TestDb { db, _dir: dir }
}

pub fn db() -> TestDb {
    open_db(&genesis())
}

/// A user record key. Never equal to a reserved key, which is ASCII zero-padded.
pub fn key(n: u8) -> RecordKey {
    RecordKey([n; 32])
}

/// A reserved record's key: its name, ASCII, zero-padded to 32 bytes.
pub fn reserved_key(name: &str) -> RecordKey {
    let mut key = [0; 32];
    key[..name.len()].copy_from_slice(name.as_bytes());
    RecordKey(key)
}

pub fn attr(value: CellValue) -> CellValue {
    value
        .with_kind(CellKind::Attribute)
        .expect("indexable value")
}

pub trait MeteredExt<T> {
    /// The value; panics with the error otherwise.
    fn ok(self) -> T;
    /// The error; panics if the call succeeded.
    fn err(self) -> ApiError;
}

impl<T: std::fmt::Debug> MeteredExt<T> for Metered<T> {
    #[track_caller]
    fn ok(self) -> T {
        self.result.expect("call failed")
    }
    #[track_caller]
    fn err(self) -> ApiError {
        self.result.expect_err("call succeeded")
    }
}

/// Asserts that an expression is an `Err` matching a pattern, e.g.
/// `assert_err!(db.get(..).result, ApiError::NotFound)`.
#[macro_export]
macro_rules! assert_err {
    ($result:expr, $pattern:pat $(if $guard:expr)?) => {
        match $result {
            Err($pattern) $(if $guard)? => {}
            other => panic!(
                "expected Err({}), got {:?}",
                stringify!($pattern),
                other.map(|_| "Ok(..)")
            ),
        }
    };
}

/// Begins a branch, applies `f`, and commits it.
pub fn in_commit(db: &Database, f: impl FnOnce(BranchId)) -> CommitId {
    let branch = db.begin().unwrap();
    f(branch);
    db.commit(branch).unwrap()
}

/// Creates one record and commits it.
pub fn commit_create(db: &Database, op: RecordOp<op::Create>) -> RecordKey {
    let mut created = None;
    in_commit(db, |b| created = Some(db.create(b, op).ok()));
    created.unwrap()
}

/// Applies one patch and commits it.
pub fn commit_patch(db: &Database, op: RecordOp<op::Patch>) {
    in_commit(db, |b| db.patch(b, op).ok());
}

/// A full read of `key` at `target`.
#[track_caller]
pub fn read(db: &Database, target: ReadTarget, key: RecordKey) -> Record {
    db.get(target, RecordOp::get(key, B)).ok()
}

/// A record's user cells by name: everything but the `#` system cells.
pub fn user_cells(record: &Record) -> BTreeMap<String, CellValue> {
    record
        .cells
        .iter()
        .filter(|(name, _)| !name.as_bytes().starts_with(b"#"))
        .map(|(name, value)| {
            (
                String::from_utf8(name.as_bytes().to_vec()).unwrap(),
                value.clone(),
            )
        })
        .collect()
}

/// Shorthand for the expected user cells of a record.
pub fn cells<const N: usize>(entries: [(&str, CellValue); N]) -> BTreeMap<String, CellValue> {
    entries
        .into_iter()
        .map(|(n, v)| (n.to_string(), v))
        .collect()
}

pub fn cell<'a>(record: &'a Record, name: &str) -> Option<&'a CellValue> {
    record.cells.get(name.as_bytes())
}

/// `#meta` decoded: `(cells, cell_bytes, indexed_cells, index_bytes)`, four
/// `u64` BE in a `bytes32` field (spec "#meta").
pub fn meta(record: &Record) -> Option<[u64; 4]> {
    let raw = cell(record, "#meta")?.as_bytes32()?;
    Some(std::array::from_fn(|i| {
        u64::from_be_bytes(raw[i * 8..i * 8 + 8].try_into().unwrap())
    }))
}

/// A cell's size as `#meta` and receipts count it: `(8 + |name|) + (1 + |value|)`.
pub fn cell_size(name: &str, value: &CellValue) -> u64 {
    (8 + name.len() + 1 + value.value().len()) as u64
}

/// An attribute's index term size: `|name| + 2 + |value|`.
pub fn term_size(name: &str, value: &CellValue) -> u64 {
    (name.len() + 2 + value.value().len()) as u64
}
