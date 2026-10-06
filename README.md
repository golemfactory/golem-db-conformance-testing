# golemdb-conformance-testing

Load tests for [Golem DB](https://github.com/golemfactory/golem-db), run through its
public API (`golemdb-api`). `golem-db/` is a submodule on `feature/golem-db-api`.

```sh
git submodule update --init
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
