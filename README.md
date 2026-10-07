# golemdb-conformance-testing

Load tests for [Golem DB](https://github.com/golemfactory/golem-db), run through its
public API (`golemdb-api`). `golem-db/` is a submodule on `feature/golem-db-api`.

## Setup

```sh
git clone --recurse-submodules <this repo>     # or, in an existing clone: git submodule update --init
```

- **Rust**: [rustup](https://rustup.rs); `rust-toolchain.toml` picks the version (golem-db's) on the first build.
- **clang**: MDBX's bindings are generated at build time (Fedora: `dnf install clang`, Ubuntu: `apt install clang`).
- **For profiling** (scenarios with `profile_at`):
  - `perf`
  - `sudo sysctl kernel.perf_event_paranoid=-1`, so perf may record this process and kernel frames
    (resets on reboot);
  - for flamegraphs, Python 3 and `cargo install inferno`.

## Running

```sh
cargo load scenarios/index-layout        # a directory runs every scenario in it, then prints a comparison
cargo load scenarios/block-size
```

## Scenario format

```toml
name = "example"
seed = 1
preload = 100000                # optional: records committed before measuring

[database]
hash = "keccak-256"             # or "blake3"
limits = { max_cell_name_len = 32, max_str_len = 64, max_bytes_len = 1024 }
# path = "target/load-db"       # wiped before each run
# max_map_size_gib = 64

[shape]                         # every record
attributes = [                  # indexed
    { name = "status", type = "str", cardinality = 4, len = 8 },
    { name = "owner", type = "u64", cardinality = 10000, distribution = "zipf" },
    { name = "tag", type = "u64", repeat = 10, distribution = "unique" },  # tag_0..tag_9
]
fields = [{ name = "payload", type = "bytes", len = 256 }]                 # not indexed

[workload]
kind = "blocks"                 # one writer, blocks of these sizes in order
blocks = [{ ops = 1000, repeat = 20 }]
mix = { create = 4, patch = 1, delete = 0 }

# A block group can also take its own `mix`, and `warmup = true` to run it unmeasured.
# commit_log = "results/x.csv"  # top level: one CSV line per commit (blocks only)
# table_log = "results/y.csv"   # top level: every MDBX table's size after each commit (blocks only)
# profile_at = [1, 100]         # top level: perf-record these commits (blocks only), see Profiling

# kind = "writers"              # N writers racing to commit; losers retry
# writers = 8
# ops_per_branch = 100
# duration_secs = 30
```

**Cell options:**

- `type`: `u64`, `str` or `bytes`. `bytes` is for fields only.
- `cardinality`: the number of distinct values. `1` means every record has the same value.
- `distribution`: `uniform` (the default), `zipf` (a few hot values), or `unique` (never repeats).
- `len`: the size of `str` and `bytes` values.

**`table_log`** reads MDBX's own statistics after every commit: entries, B-tree depth, pages and MiB for each
table (`Cell`, `CellTrie`, `Index`, ...), plus the file space outside the tables: `free` (pages released by
earlier commits, waiting for reuse), `other` (MDBX metadata), `unused` (grown ahead of need) and `file` (the
total). It is read by a separate read-only process between commits, so it is not timed, but it does look at
the storage file directly rather than through the API.

## Profiling

`profile_at` records the listed commits with `perf` (one file per commit in `results/profiles/`), so you can
compare flamegraphs at different database sizes. Profiled commits are marked in the commit log
(`profiled` = 1) and left out of the printed stats, since perf slows them down. perf is checked before the
first scenario starts. Every build has frame pointers (`.cargo/config.toml`), so stacks unwind:

```sh
cargo load scenarios/large/large-realistic.toml
scripts/flamegraph.py results/profiles/large-realistic-commit250.perf.data results/large-realistic.csv 250
```

The flamegraph puts the work of libmdbx's writer thread under `commit`, and adds a `[waiting]` bar for time not
spent on the CPU (wall time from the commit log minus sampled CPU time), so widths add up to real time.
