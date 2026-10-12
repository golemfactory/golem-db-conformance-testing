use anyhow::{Context, Result, bail};
use golemdb_api::{CellLimits, HashAlgorithm};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    /// One line on what the scenario tests; printed with its results.
    #[serde(default)]
    pub description: String,
    pub seed: u64,
    pub database: Database,
    pub shape: Shape,
    /// Records committed (in blocks of 10k) before measuring.
    #[serde(default)]
    pub preload: u64,
    /// `blocks` only: write one CSV line per commit to this path.
    #[serde(default)]
    pub commit_log: Option<String>,
    /// `blocks` only: after every commit, write each MDBX table's size to this CSV.
    #[serde(default)]
    pub table_log: Option<String>,
    /// Write `table_log` only every N commits (each read costs a process and a scan).
    #[serde(default = "one_u64")]
    pub table_log_every: u64,
    /// `blocks` only: `perf record` these commits (1-based) to
    /// `results/profiles/<name>-commit<N>.perf.data`; see scripts/flamegraph.py.
    #[serde(default)]
    pub profile_at: Vec<u64>,
    pub workload: Workload,
}

impl Scenario {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        let mut scenario: Self =
            toml::from_str(&text).with_context(|| path.display().to_string())?;
        // Puts every scenario's database there instead, e.g. on another disk.
        if let Ok(dir) = std::env::var("GOLEMDB_DB_DIR") {
            scenario.database.path = dir;
        }
        if scenario.table_log_every == 0 {
            bail!("{}: `table_log_every` must be at least 1", path.display());
        }
        for cell in scenario
            .shape
            .attributes
            .iter()
            .chain(&scenario.shape.fields)
        {
            if let Len::Ranges(ranges) = &cell.len
                && (ranges.iter().map(|r| r.weight).sum::<u32>() == 0
                    || ranges.iter().any(|r| r.min > r.max))
            {
                bail!(
                    "{}: `{}`: `len` ranges need min <= max and some weight",
                    path.display(),
                    cell.name
                );
            }
        }
        Ok(scenario)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Database {
    /// MDBX directory; deleted and recreated for every run.
    #[serde(default = "default_path")]
    pub path: String,
    pub hash: HashAlgorithm,
    pub limits: CellLimits,
    #[serde(default = "default_map_size_gib")]
    pub max_map_size_gib: u64,
}

fn default_path() -> String {
    "target/load-db".into()
}
fn default_map_size_gib() -> u64 {
    64
}

/// What every generated record looks like.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    /// Indexed cells.
    #[serde(default)]
    pub attributes: Vec<Cell>,
    /// Unindexed cells.
    #[serde(default)]
    pub fields: Vec<Cell>,
    /// Patches rewrite every cell except `unique` ones (an entity's ids stay),
    /// like Arkiv's update; otherwise a patch sets one random cell.
    #[serde(default)]
    pub patch_all_cells: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Type,
    /// Number of distinct values (ignored by `unique`).
    #[serde(default = "default_cardinality")]
    pub cardinality: u64,
    #[serde(default)]
    pub distribution: Distribution,
    /// Byte length for `str` and `bytes`: a number, or weighted ranges (see `Len`).
    #[serde(default = "default_len")]
    pub len: Len,
    /// Expands into `name_0 .. name_{repeat-1}`.
    #[serde(default = "one")]
    pub repeat: usize,
}

fn default_cardinality() -> u64 {
    1000
}
fn default_len() -> Len {
    Len::Fixed(32)
}

/// A fixed length, or weighted ranges drawn uniformly, e.g.
/// `[{ min = 1024, max = 4096, weight = 90 }, { min = 4096, max = 102400, weight = 10 }]`.
#[derive(Deserialize, Clone)]
#[serde(untagged)]
pub enum Len {
    Fixed(usize),
    Ranges(Vec<LenRange>),
}

#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LenRange {
    pub min: usize,
    pub max: usize,
    pub weight: u32,
}
fn one() -> usize {
    1
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Type {
    U64,
    Str,
    /// Field-only.
    Bytes,
}

#[derive(Deserialize, Clone, Copy, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Distribution {
    #[default]
    Uniform,
    /// A few hot values hold most records.
    Zipf,
    /// Never repeats: one posting list per record.
    Unique,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Workload {
    /// One writer commits blocks of the given sizes, in order.
    Blocks {
        blocks: Vec<Blocks>,
        #[serde(default)]
        mix: Mix,
    },
    /// N writers race to commit; losers retry on the new head.
    Writers {
        writers: usize,
        ops_per_branch: usize,
        duration_secs: u64,
        #[serde(default)]
        mix: Mix,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blocks {
    pub ops: usize,
    #[serde(default = "one")]
    pub repeat: usize,
    /// Overrides the workload's `mix` for this group of blocks.
    #[serde(default)]
    pub mix: Option<Mix>,
    /// Run these blocks but leave them out of every result (e.g. the first
    /// commit after a preload, which is often unusually fast).
    #[serde(default)]
    pub warmup: bool,
}

/// Relative weights of operation kinds. Patches and deletes target records
/// the same writer committed earlier.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mix {
    #[serde(default = "one_u32")]
    pub create: u32,
    #[serde(default)]
    pub patch: u32,
    #[serde(default)]
    pub delete: u32,
}

fn one_u32() -> u32 {
    1
}
fn one_u64() -> u64 {
    1
}

impl Default for Mix {
    fn default() -> Self {
        Self {
            create: 1,
            patch: 0,
            delete: 0,
        }
    }
}
