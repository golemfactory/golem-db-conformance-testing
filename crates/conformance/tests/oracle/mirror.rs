//! The SQLite oracle: what the spec says golem-db's state and answers must be.
//!
//! Committed state is one in-memory SQLite database; each branch is a copy of
//! it taken at `begin`, with one SAVEPOINT per frame. `commit` makes the branch's
//! copy the new committed state. Queries are SQL over the committed copy.
//! Every rule below is from the spec (API spec sections cited), not from golem-db.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::time::Duration;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, backup::Backup, params};

use crate::value::{Tv, Ty};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Field,
    Attribute,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub value: Tv,
    pub kind: Kind,
}

/// A record's user cells by name.
pub type Cells = BTreeMap<String, Cell>;

#[derive(Debug, Clone)]
pub enum Change {
    Write(Tv, Kind),
    Set(Tv),
    Remove,
}

/// The failures the spec prescribes, by `ApiError` variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expected {
    HandleInvalid,
    Sealed,
    InvalidArgument,
    AlreadyExists,
    NotFound,
    CellNotFound(String),
    NoFrameToRollback,
    InvalidQuery,
    /// `commit` on a branch it is the first call to find stale: `HandleInvalid`.
    CommitLost,
}

/// The receipt's `Details`, in field order: cells created, updated, deleted,
/// index joins, leaves, cell bytes written, deleted, index bytes written, deleted.
pub type Effects = [u64; 9];

/// `#meta`: cells, cell bytes, indexed cells, index bytes.
pub fn meta(cells: &Cells) -> [u64; 4] {
    let attrs = || cells.iter().filter(|(_, c)| c.kind == Kind::Attribute);
    [
        cells.len() as u64,
        cells.iter().map(|(n, c)| cell_size(n, c)).sum(),
        attrs().count() as u64,
        attrs().map(|(n, c)| term_size(n, c)).sum(),
    ]
}

/// "`(8 + |name|) + (1 + |value|)`"
fn cell_size(name: &str, cell: &Cell) -> u64 {
    (8 + name.len() + 1 + cell.value.key().len()) as u64
}

/// "`|name| + 2 + |value|`"
fn term_size(name: &str, cell: &Cell) -> u64 {
    (name.len() + 2 + cell.value.key().len()) as u64
}

/// Receipts, "Effects, not requests": the effects of turning `before` into `after`.
fn effects(before: &Cells, after: &Cells) -> Effects {
    let mut e = [0; 9];
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let leave = |e: &mut Effects, n: &str, c: &Cell| {
        if c.kind == Kind::Attribute {
            e[4] += 1;
            e[8] += term_size(n, c);
        }
    };
    let join = |e: &mut Effects, n: &str, c: &Cell| {
        if c.kind == Kind::Attribute {
            e[3] += 1;
            e[7] += term_size(n, c);
        }
    };
    for name in names {
        match (before.get(name), after.get(name)) {
            (None, Some(new)) => {
                e[0] += 1;
                e[5] += cell_size(name, new);
                join(&mut e, name, new);
            }
            (Some(old), None) => {
                e[2] += 1;
                e[6] += cell_size(name, old);
                leave(&mut e, name, old);
            }
            (Some(old), Some(new)) if !same(old, new) => {
                e[1] += 1;
                e[6] += cell_size(name, old);
                e[5] += cell_size(name, new);
                leave(&mut e, name, old);
                join(&mut e, name, new);
            }
            _ => {}
        }
    }
    e
}

/// "equal to the new value (type, kind and payload)"
fn same(a: &Cell, b: &Cell) -> bool {
    a.kind == b.kind && a.value.ty() == b.value.ty() && a.value.key() == b.value.key()
}

pub struct Limits {
    pub max_cell_name_len: usize,
    pub max_str_len: usize,
    pub max_bytes_len: usize,
    pub max_cells: u64,
    pub max_indexed_cells: u64,
}

struct Branch {
    /// `None` once consumed (committed, discarded, or found stale).
    conn: Option<Connection>,
    origin: u64,
    sealed: bool,
    /// Open frames; frame `i` is SAVEPOINT `f{i}`.
    frames: usize,
    /// The current frame was just restored, with no write or checkpoint since.
    restored: bool,
}

pub struct Mirror {
    head: Connection,
    pub head_commit: u64,
    branches: Vec<Branch>,
    limits: Limits,
}

const SCHEMA: &str = "
    CREATE TABLE records (key BLOB PRIMARY KEY, id INTEGER NOT NULL);
    CREATE TABLE cells (key BLOB, name BLOB, type INTEGER, attr INTEGER, value BLOB,
                        PRIMARY KEY (key, name));
    CREATE TABLE alloc (next INTEGER NOT NULL);
    INSERT INTO alloc VALUES (64);
";

fn copy(src: &Connection) -> Connection {
    let mut dst = Connection::open_in_memory().unwrap();
    Backup::new(src, &mut dst)
        .unwrap()
        .run_to_completion(1000, Duration::ZERO, None)
        .unwrap();
    dst
}

fn exec(conn: &Connection, sql: &str) {
    conn.execute_batch(sql).unwrap();
}

/// The user-name grammar and `#maxCellNameLen` (spec "Cell names").
fn valid_name(name: &str, max: usize) -> bool {
    let rest = name.strip_prefix('$').unwrap_or(name);
    let mut chars = rest.chars();
    !name.is_empty()
        && name.len() <= max
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_-.:".contains(c))
}

impl Mirror {
    pub fn new(limits: Limits) -> Self {
        let head = Connection::open_in_memory().unwrap();
        exec(&head, SCHEMA);
        Self {
            head,
            head_commit: 0,
            branches: Vec::new(),
            limits,
        }
    }

    // ---- reading state -------------------------------------------------

    fn record(conn: &Connection, key: &[u8]) -> Option<Cells> {
        conn.query_row("SELECT id FROM records WHERE key = ?", [key], |_| Ok(()))
            .optional()
            .unwrap()?;
        let mut stmt = conn
            .prepare("SELECT name, type, attr, value FROM cells WHERE key = ?")
            .unwrap();
        let cells = stmt
            .query_map([key], |row| {
                let name: Vec<u8> = row.get(0)?;
                let ty: u8 = row.get(1)?;
                let attr: bool = row.get(2)?;
                let value: Vec<u8> = row.get(3)?;
                Ok((
                    String::from_utf8(name).unwrap(),
                    Cell {
                        value: Tv::from_key(Ty::from_id(ty), &value),
                        kind: if attr { Kind::Attribute } else { Kind::Field },
                    },
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Some(cells)
    }

    fn next_id(conn: &Connection) -> u64 {
        conn.query_row("SELECT next FROM alloc", [], |r| r.get::<_, i64>(0))
            .unwrap() as u64
    }

    /// Committed state of `key`.
    pub fn head_record(&self, key: &[u8]) -> Option<Cells> {
        Self::record(&self.head, key)
    }

    pub fn head_next_id(&self) -> u64 {
        Self::next_id(&self.head)
    }

    /// A branch's state of `key`, if the branch is open and not stale. Never
    /// consumes the branch: for state comparisons, not for `get` calls.
    fn peek(&self, b: usize) -> Option<&Connection> {
        let branch = &self.branches[b];
        branch
            .conn
            .as_ref()
            .filter(|_| branch.origin == self.head_commit)
    }

    pub fn peek_record(&self, b: usize, key: &[u8]) -> Option<Option<Cells>> {
        self.peek(b).map(|conn| Self::record(conn, key))
    }

    /// The keys bound in a branch (`Some`) or in committed state (`None`).
    pub fn keys(&self, b: Option<usize>) -> Vec<Vec<u8>> {
        let conn = match b {
            Some(b) => match self.peek(b) {
                Some(conn) => conn,
                None => return Vec::new(),
            },
            None => &self.head,
        };
        let mut stmt = conn.prepare("SELECT key FROM records ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Every committed attribute cell, as (name, value).
    pub fn head_attributes(&self) -> Vec<(String, Tv)> {
        let mut stmt = self
            .head
            .prepare("SELECT name, type, value FROM cells WHERE attr = 1 ORDER BY key, name")
            .unwrap();
        stmt.query_map([], |r| {
            let name: Vec<u8> = r.get(0)?;
            let ty: u8 = r.get(1)?;
            let value: Vec<u8> = r.get(2)?;
            Ok((
                String::from_utf8(name).unwrap(),
                Tv::from_key(Ty::from_id(ty), &value),
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    pub fn peek_next_id(&self, b: usize) -> Option<u64> {
        self.peek(b).map(Self::next_id)
    }

    // ---- branches (spec "Branches") ------------------------------------

    pub fn begin(&mut self) -> usize {
        let conn = copy(&self.head);
        exec(&conn, "SAVEPOINT f0");
        self.branches.push(Branch {
            conn: Some(conn),
            origin: self.head_commit,
            sealed: false,
            frames: 1,
            restored: false,
        });
        self.branches.len() - 1
    }

    pub fn branch_count(&self) -> usize {
        self.branches.len()
    }

    /// The commit a branch was opened on, which prices its calls.
    pub fn origin(&self, b: usize) -> u64 {
        self.branches[b].origin
    }

    /// "Every branch call first validates the handle against the current head":
    /// unknown or consumed → `HandleInvalid`; stale → `HandleInvalid`, and "the
    /// first call that detects it removes the branch".
    fn live(&mut self, b: usize, lost: Expected) -> Result<&mut Branch, Expected> {
        let head = self.head_commit;
        let branch = &mut self.branches[b];
        if branch.conn.is_none() {
            return Err(Expected::HandleInvalid);
        }
        if branch.origin != head {
            branch.conn = None;
            return Err(lost);
        }
        Ok(branch)
    }

    fn writable(&mut self, b: usize) -> Result<&mut Branch, Expected> {
        let branch = self.live(b, Expected::HandleInvalid)?;
        if branch.sealed {
            return Err(Expected::Sealed);
        }
        Ok(branch)
    }

    /// "ends the current frame and opens a new one". Right after a rollback it
    /// instead makes the restored marker usable again ("a write or checkpoint
    /// makes the last marker usable again"): the spec's two sentences conflict
    /// there, and golem-db reads it this way.
    pub fn checkpoint(&mut self, b: usize) -> Result<(), Expected> {
        let branch = self.writable(b)?;
        if !branch.restored {
            exec(
                branch.conn.as_ref().unwrap(),
                &format!("SAVEPOINT f{}", branch.frames),
            );
            branch.frames += 1;
        }
        branch.restored = false;
        Ok(())
    }

    /// "A rollback restores the current frame's starting state and keeps its
    /// marker. Another rollback without an intervening write or checkpoint steps
    /// back to the preceding frame. … Once the first frame has been rolled back,
    /// a further rollback is `NoFrameToRollback`"
    pub fn rollback(&mut self, b: usize) -> Result<(), Expected> {
        let branch = self.writable(b)?;
        let conn = branch.conn.as_ref().unwrap();
        if branch.restored {
            if branch.frames == 1 {
                return Err(Expected::NoFrameToRollback);
            }
            branch.frames -= 1;
            exec(conn, &format!("RELEASE f{}", branch.frames));
        }
        exec(conn, &format!("ROLLBACK TO f{}", branch.frames - 1));
        branch.restored = true;
        Ok(())
    }

    pub fn seal(&mut self, b: usize) -> Result<(), Expected> {
        self.live(b, Expected::HandleInvalid)?.sealed = true;
        Ok(())
    }

    /// "seals if needed, then persists atomically and advances the head;
    /// consumes the branch", and every other branch over the old head is stale.
    pub fn commit(&mut self, b: usize) -> Result<u64, Expected> {
        let branch = self.live(b, Expected::CommitLost)?;
        let conn = branch.conn.take().unwrap();
        exec(&conn, "RELEASE f0");
        self.head = conn;
        self.head_commit += 1;
        Ok(self.head_commit)
    }

    pub fn discard(&mut self, b: usize) -> Result<(), Expected> {
        self.live(b, Expected::HandleInvalid)?.conn = None;
        Ok(())
    }

    // ---- records (spec "Record operations") ----------------------------

    fn admit_value(&self, value: &Tv, kind: Kind) -> Result<(), Expected> {
        let ok = match value {
            Tv::Bytes(b) => kind == Kind::Field && b.len() <= self.limits.max_bytes_len,
            Tv::Str(s) => s.len() <= self.limits.max_str_len,
            _ => true,
        };
        ok.then_some(()).ok_or(Expected::InvalidArgument)
    }

    fn admit_names<'a>(&self, names: impl Iterator<Item = &'a String>) -> Result<(), Expected> {
        let mut seen = BTreeSet::new();
        for name in names {
            if !valid_name(name, self.limits.max_cell_name_len) || !seen.insert(name) {
                return Err(Expected::InvalidArgument);
            }
        }
        Ok(())
    }

    /// Per-record caps (metering D5): checked on the resulting counts.
    fn within_caps(&self, cells: &Cells) -> Result<(), Expected> {
        let [count, _, indexed, _] = meta(cells);
        (count <= self.limits.max_cells && indexed <= self.limits.max_indexed_cells)
            .then_some(())
            .ok_or(Expected::InvalidArgument)
    }

    fn store(conn: &Connection, key: &[u8], cells: &Cells) {
        conn.execute("DELETE FROM cells WHERE key = ?", [key])
            .unwrap();
        for (name, cell) in cells {
            conn.execute(
                "INSERT INTO cells VALUES (?, ?, ?, ?, ?)",
                params![
                    key,
                    name.as_bytes(),
                    cell.value.ty().id(),
                    cell.kind == Kind::Attribute,
                    cell.value.key()
                ],
            )
            .unwrap();
        }
    }

    /// Spec `create`: handle, admission, record state.
    pub fn create(
        &mut self,
        b: usize,
        key: &[u8],
        cells: &[(String, Tv, Kind)],
    ) -> Result<Effects, Expected> {
        self.writable(b)?;
        self.admit_names(cells.iter().map(|(n, ..)| n))?;
        for (_, value, kind) in cells {
            self.admit_value(value, *kind)?;
        }
        let new: Cells = cells
            .iter()
            .map(|(n, v, k)| {
                (
                    n.clone(),
                    Cell {
                        value: v.clone(),
                        kind: *k,
                    },
                )
            })
            .collect();
        let conn = self.branches[b].conn.as_ref().unwrap();
        if Self::record(conn, key).is_some() {
            return Err(Expected::AlreadyExists);
        }
        self.within_caps(&new)?;
        let branch = &mut self.branches[b];
        let conn = branch.conn.as_ref().unwrap();
        let id = Self::next_id(conn);
        conn.execute("INSERT INTO records VALUES (?, ?)", params![key, id as i64])
            .unwrap();
        conn.execute("UPDATE alloc SET next = next + 1", [])
            .unwrap();
        Self::store(conn, key, &new);
        branch.restored = false;
        Ok(effects(&Cells::new(), &new))
    }

    /// Spec `patch`, row by row of its effect table, in name order.
    pub fn patch(
        &mut self,
        b: usize,
        key: &[u8],
        changes: &[(String, Change)],
    ) -> Result<Effects, Expected> {
        self.writable(b)?;
        self.admit_names(changes.iter().map(|(n, _)| n))?;
        for (_, change) in changes {
            match change {
                Change::Write(value, kind) => self.admit_value(value, *kind)?,
                // Length limits only: the kind is the stored cell's, known below.
                Change::Set(value) => self.admit_value(value, Kind::Field)?,
                Change::Remove => {}
            }
        }
        let conn = self.branches[b].conn.as_ref().unwrap();
        let before = Self::record(conn, key).ok_or(Expected::NotFound)?;
        let mut after = before.clone();
        let mut sorted: Vec<_> = changes.iter().collect();
        sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        for (name, change) in sorted {
            match change {
                Change::Write(value, kind) => {
                    after.insert(
                        name.clone(),
                        Cell {
                            value: value.clone(),
                            kind: *kind,
                        },
                    );
                }
                Change::Set(value) => {
                    let kind = before
                        .get(name)
                        .ok_or(Expected::CellNotFound(name.clone()))?
                        .kind;
                    // "a value that … cannot take the kind (`bytes` as attribute)"
                    if kind == Kind::Attribute && value.ty() == Ty::Bytes {
                        return Err(Expected::InvalidArgument);
                    }
                    after.insert(
                        name.clone(),
                        Cell {
                            value: value.clone(),
                            kind,
                        },
                    );
                }
                Change::Remove => {
                    after.remove(name);
                }
            }
        }
        self.within_caps(&after)?;
        let effects = effects(&before, &after);
        // A patch that changes nothing is not "a write" for rollback: it leaves
        // no undo entry ("an identical write adds none"). The spec does not say
        // this outright; golem-db behaves so.
        if effects != [0; 9] {
            let branch = &mut self.branches[b];
            Self::store(branch.conn.as_ref().unwrap(), key, &after);
            branch.restored = false;
        }
        Ok(effects)
    }

    pub fn delete(&mut self, b: usize, key: &[u8]) -> Result<Effects, Expected> {
        self.writable(b)?;
        let branch = &mut self.branches[b];
        let conn = branch.conn.as_ref().unwrap();
        let before = Self::record(conn, key).ok_or(Expected::NotFound)?;
        conn.execute("DELETE FROM records WHERE key = ?", [key])
            .unwrap();
        conn.execute("DELETE FROM cells WHERE key = ?", [key])
            .unwrap();
        branch.restored = false;
        Ok(effects(&before, &Cells::new()))
    }

    /// Spec `get` on a branch: "the target first … then the binding (`NotFound`)".
    pub fn get_branch(&mut self, b: usize, key: &[u8]) -> Result<Cells, Expected> {
        let branch = self.live(b, Expected::HandleInvalid)?;
        Self::record(branch.conn.as_ref().unwrap(), key).ok_or(Expected::NotFound)
    }

    // ---- query and count (query proposal) ------------------------------

    /// The keys `filter` matches in committed state, in record ID order, and the
    /// size of the match set. `Err` lists every error admission could report.
    fn admit(&self, filter: &Qf) -> Result<(), Vec<Expected>> {
        let mut problems = Vec::new();
        filter.admit(self.limits.max_cell_name_len, &mut problems);
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }

    /// The size of the match set, as `count` returns it.
    pub fn count(&self, filter: &Qf) -> Result<u64, Vec<Expected>> {
        self.admit(filter)?;
        let mut sql = String::new();
        let mut args = Vec::new();
        filter.sql(&mut sql, &mut args);
        let total: i64 = self
            .head
            .query_row(
                &format!("SELECT COUNT(*) FROM records r WHERE {sql}"),
                rusqlite::params_from_iter(&args),
                |r| r.get(0),
            )
            .unwrap();
        Ok(total as u64)
    }

    pub fn query(
        &self,
        filter: &Qf,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<(Vec<Vec<u8>>, u64), Vec<Expected>> {
        let mut problems = self.admit(filter).err().unwrap_or_default();
        if limit == Some(0) {
            problems.push(Expected::InvalidQuery);
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let total = self.count(filter)?;
        let mut sql = String::new();
        let mut args = Vec::new();
        filter.sql(&mut sql, &mut args);
        args.push(Value::Integer(
            limit.map_or(-1, |l| l.min(i64::MAX as u64) as i64),
        ));
        args.push(Value::Integer(offset.min(i64::MAX as u64) as i64));
        let mut stmt = self
            .head
            .prepare(&format!(
                "SELECT r.key FROM records r WHERE {sql} ORDER BY r.id LIMIT ? OFFSET ?"
            ))
            .unwrap();
        let keys = stmt
            .query_map(rusqlite::params_from_iter(&args), |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        Ok((keys, total))
    }
}

/// A query filter (query proposal "AST query format"), over oracle values.
#[derive(Debug, Clone)]
pub enum Qf {
    Eq(String, Tv),
    In(String, Ty, Vec<Tv>),
    Range(String, Ty, Bound<Tv>, Bound<Tv>),
    Prefix(String, String),
    Exists(String, Option<Ty>),
    And(Vec<Qf>),
    Or(Vec<Qf>),
}

impl Qf {
    /// Admission: names checked, `And` and `Or` not empty.
    fn admit(&self, max_name: usize, problems: &mut Vec<Expected>) {
        match self {
            Qf::Eq(n, _)
            | Qf::In(n, ..)
            | Qf::Range(n, ..)
            | Qf::Prefix(n, _)
            | Qf::Exists(n, _) => {
                if !valid_name(n, max_name) {
                    problems.push(Expected::InvalidArgument);
                }
            }
            Qf::And(children) | Qf::Or(children) => {
                if children.is_empty() {
                    problems.push(Expected::InvalidQuery);
                }
                children.iter().for_each(|c| c.admit(max_name, problems));
            }
        }
    }

    /// A SQL condition on `records r`. Leaves match attribute cells of their
    /// name and type only ("a cell of another type under the same name is absent").
    fn sql(&self, out: &mut String, args: &mut Vec<Value>) {
        let leaf = |out: &mut String,
                    args: &mut Vec<Value>,
                    name: &str,
                    ty: Option<Ty>,
                    cond: &str,
                    extra: Vec<Value>| {
            out.push_str(
                "EXISTS (SELECT 1 FROM cells c WHERE c.key = r.key AND c.attr = 1 AND c.name = ?",
            );
            args.push(Value::Blob(name.as_bytes().to_vec()));
            if let Some(ty) = ty {
                out.push_str(" AND c.type = ?");
                args.push(Value::Integer(ty.id().into()));
            }
            out.push_str(cond);
            out.push(')');
            args.extend(extra);
        };
        match self {
            Qf::Eq(n, v) => leaf(
                out,
                args,
                n,
                Some(v.ty()),
                " AND c.value = ?",
                vec![Value::Blob(v.key())],
            ),
            Qf::In(n, ty, values) => {
                if values.is_empty() {
                    out.push('0');
                } else {
                    let marks = vec!["?"; values.len()].join(", ");
                    leaf(
                        out,
                        args,
                        n,
                        Some(*ty),
                        &format!(" AND c.value IN ({marks})"),
                        values.iter().map(|v| Value::Blob(v.key())).collect(),
                    );
                }
            }
            Qf::Range(n, ty, lo, hi) => {
                let mut cond = String::new();
                let mut extra = Vec::new();
                for (bound, inc, exc) in [(lo, ">=", ">"), (hi, "<=", "<")] {
                    match bound {
                        Bound::Included(v) => {
                            cond.push_str(&format!(" AND c.value {inc} ?"));
                            extra.push(Value::Blob(v.key()));
                        }
                        Bound::Excluded(v) => {
                            cond.push_str(&format!(" AND c.value {exc} ?"));
                            extra.push(Value::Blob(v.key()));
                        }
                        Bound::Unbounded => {}
                    }
                }
                leaf(out, args, n, Some(*ty), &cond, extra);
            }
            Qf::Prefix(n, p) => {
                if p.is_empty() {
                    leaf(out, args, n, Some(Ty::Str), "", vec![]);
                } else {
                    leaf(
                        out,
                        args,
                        n,
                        Some(Ty::Str),
                        " AND substr(c.value, 1, ?) = ?",
                        vec![
                            Value::Integer(p.len() as i64),
                            Value::Blob(p.as_bytes().to_vec()),
                        ],
                    );
                }
            }
            Qf::Exists(n, ty) => leaf(out, args, n, *ty, "", vec![]),
            Qf::And(children) | Qf::Or(children) => {
                let op = if matches!(self, Qf::And(_)) {
                    " AND "
                } else {
                    " OR "
                };
                out.push('(');
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        out.push_str(op);
                    }
                    child.sql(out, args);
                }
                out.push(')');
            }
        }
    }
}
