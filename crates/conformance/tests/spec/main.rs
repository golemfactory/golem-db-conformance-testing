//! Spec conformance: one module per section of golem-db's `docs/golemdb-api.md`.
//! Each test quotes the statement it checks. Tests for what the spec states
//! but `main` does not implement yet are `#[ignore = "spec-gap: …"]`; list
//! them with `cargo test -p golemdb-conformance -- --ignored`.

mod branches;
mod cell_names;
mod cell_types;
mod create;
mod delete;
mod get;
mod meta;
mod order_of_checks;
mod patch;
mod query;
mod receipts;
mod record_keys;
mod record_op;
mod records;
mod reserved_records;
