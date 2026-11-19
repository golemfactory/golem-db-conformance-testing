//! Running a scenario's queries, and the `queries` workload: fill the
//! database in steps, run every query after each step.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::Bound;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use golemdb_api::{
    Bounds, Budget, CellNameRef, CellValue, CountOp, EqValue, Filter, InValues, QueryOp,
};
use hdrhistogram::Histogram;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::Db;
use crate::config::{self, Query, Rank, Scenario};
use crate::generate::Generator;
use crate::workloads::{Pool, csv, fill};

/// A query stops repeating after this long, even short of `runs`.
const MAX_TIME_PER_QUERY: Duration = Duration::from_secs(10);

/// Runs a scenario's queries against the database as it stands, and logs them.
pub struct Runner<'a> {
    queries: &'a [Query],
    runs: usize,
    /// Draws `random` ranks; separate from the generator, so queries never change what is written.
    rng: ChaCha8Rng,
    log: Option<BufWriter<File>>,
    width: usize,
}

impl<'a> Runner<'a> {
    /// Builds every query once, so a bad filter fails before anything runs.
    pub fn new(
        scenario: &Scenario,
        queries: &'a [Query],
        runs: usize,
        generator: &Generator,
    ) -> Result<Self> {
        let mut rng = ChaCha8Rng::seed_from_u64(scenario.seed);
        for query in queries {
            op(query, generator, &mut rng).map_err(|e| anyhow!("query `{}`: {e}", query.name))?;
        }
        Ok(Self {
            queries,
            runs,
            rng,
            log: csv(
                &scenario.query_log,
                "commit,records,query,matches,returned,runs,first_ms,p50_ms,p99_ms,max_ms",
            )?,
            width: queries
                .iter()
                .map(|q| q.name.len())
                .max()
                .unwrap_or(0)
                .max(5),
        })
    }

    /// Runs every query up to `runs` times. `commit` is the measured commit
    /// the database is at, if the workload counts them.
    pub fn run(
        &mut self,
        db: &Db,
        generator: &Generator,
        commit: Option<u64>,
        records: u64,
    ) -> Result<()> {
        let width = self.width;
        println!(
            "    {:<width$} {:>9} {:>8} {:>10} {:>10} {:>10} {:>10} {:>5}",
            "query", "matches", "returned", "first ms", "p50 ms", "p99 ms", "max ms", "runs"
        );
        for query in self.queries {
            let mut latency = Histogram::<u64>::new(3)?;
            let mut matches = Vec::new();
            let (mut first, mut returned) = (Duration::ZERO, Vec::new());
            let start = Instant::now();
            while (latency.len() as usize) < self.runs && start.elapsed() < MAX_TIME_PER_QUERY {
                // Built outside the timer: only the engine call is measured.
                let op = op(query, generator, &mut self.rng)?;
                let t = Instant::now();
                let (took, matched) = match op {
                    Op::Query(op) => {
                        let page = db.query(None, op).into_result()?;
                        let took = t.elapsed();
                        returned.push(page.records.len());
                        (took, page.total_matched.unwrap_or(0))
                    }
                    Op::Count(op) => {
                        let count = db.count(None, op).into_result()?;
                        (t.elapsed(), count)
                    }
                };
                if latency.is_empty() {
                    first = took;
                }
                latency.record((took.as_micros() as u64).max(1))?;
                matches.push(matched);
            }
            // Medians, as `random` values match a different number each run.
            let matches = median(matches);
            let returned = median(returned);
            let ms = |micros: u64| micros as f64 / 1000.0;
            let (first, p50, p99, max) = (
                first.as_secs_f64() * 1000.0,
                ms(latency.value_at_quantile(0.5)),
                ms(latency.value_at_quantile(0.99)),
                ms(latency.max()),
            );
            println!(
                "    {:<width$} {matches:>9} {returned:>8} {first:>10.3} {p50:>10.3} {p99:>10.3} {max:>10.3} {:>5}",
                query.name,
                latency.len()
            );
            if let Some(file) = &mut self.log {
                writeln!(
                    file,
                    "{},{records},\"{}\",{matches},{returned},{},{first:.3},{p50:.3},{p99:.3},{max:.3}",
                    commit.map(|c| c.to_string()).unwrap_or_default(),
                    // Quoted: names may hold commas.
                    query.name.replace('"', "\"\""),
                    latency.len()
                )?;
                file.flush()?;
            }
        }
        Ok(())
    }
}

/// The `queries` workload. `at` counts every record in the database, `preload` included.
pub fn run(db: &Db, scenario: &Scenario, at: &[u64], runs: usize, queries: &[Query]) -> Result<()> {
    let mut generator = Generator::new(&scenario.shape, scenario.seed, 0)?;
    let mut pool = Pool::new(scenario.seed, 0);
    let mut runner = Runner::new(scenario, queries, runs, &generator)?;
    let mut records = scenario.preload;
    for &size in at {
        if size <= records {
            bail!("`at` must be ascending and above `preload` ({size} <= {records})");
        }
        let start = Instant::now();
        fill(db, &mut pool, &mut generator, size - records)?;
        println!(
            "  {size} records (filled in {:.1}s, disk {:.0} MiB)",
            start.elapsed().as_secs_f64(),
            crate::dir_size(Path::new(&scenario.database.path)) as f64 / (1 << 20) as f64
        );
        records = size;
        runner.run(db, &generator, None, records)?;
    }
    Ok(())
}

enum Op {
    Query(QueryOp),
    Count(CountOp),
}

/// The operation `query` runs, with fresh values for its `random` ranks.
/// Pages always ask for the total, to report `matches`: the whole match set
/// is built anyway, so it costs nothing.
fn op(query: &Query, generator: &Generator, rng: &mut ChaCha8Rng) -> Result<Op> {
    let filter = build(&query.filter, generator, rng)?;
    if query.count {
        return Ok(Op::Count(CountOp::filter(filter, Budget::Unlimited)));
    }
    let mut op = QueryOp::from_filter(filter, Budget::Unlimited).with_total();
    if let Some(limit) = query.limit {
        op = op.limit(limit);
    }
    if let Some(offset) = query.offset {
        op = op.offset(offset);
    }
    if let Some(only) = &query.only {
        op = op.only(only);
    }
    Ok(Op::Query(op))
}

fn build(spec: &config::Filter, generator: &Generator, rng: &mut ChaCha8Rng) -> Result<Filter> {
    let field = |name: &str| CellNameRef::raw(name.as_bytes()).into();
    let mut value = |name: &str, rank: Rank| -> Result<CellValue> {
        let n = match rank {
            Rank::Fixed(n) => n,
            Rank::Random(_) => generator.random_rank(name, rng)?,
        };
        generator.value_at(name, n)
    };
    Ok(match spec {
        config::Filter::Eq(leaf) => {
            let (name, &rank) = single(leaf)?;
            Filter::Eq {
                field: field(name),
                value: EqValue::try_from(value(name, rank)?).map_err(|e| anyhow!("{e:?}"))?,
            }
        }
        config::Filter::In(leaf) => {
            let (name, ranks) = single(leaf)?;
            let values = ranks
                .iter()
                .map(|&rank| value(name, rank))
                .collect::<Result<Vec<_>>>()?;
            Filter::In {
                field: field(name),
                values: InValues::try_from(values).map_err(|e| anyhow!("{e:?}"))?,
            }
        }
        config::Filter::Range(leaf) => {
            let (name, b) = single(leaf)?;
            let mut bound =
                |included: Option<Rank>, excluded: Option<Rank>| -> Result<Bound<CellValue>> {
                    Ok(match (included, excluded) {
                        (Some(_), Some(_)) => {
                            bail!("`{name}`: a side is either inclusive or exclusive")
                        }
                        (Some(rank), None) => Bound::Included(value(name, rank)?),
                        (None, Some(rank)) => Bound::Excluded(value(name, rank)?),
                        (None, None) => Bound::Unbounded,
                    })
                };
            let (lower, upper) = (bound(b.gte, b.gt)?, bound(b.lte, b.lt)?);
            Filter::Range {
                field: field(name),
                bounds: Bounds::try_new(generator.type_of(name)?, lower, upper)
                    .map_err(|e| anyhow!("{e:?}"))?,
            }
        }
        config::Filter::Prefix(leaf) => {
            let (name, prefix) = single(leaf)?;
            generator.type_of(name)?;
            Filter::Prefix {
                field: field(name),
                prefix: prefix.clone(),
            }
        }
        config::Filter::Exists(name) => {
            generator.type_of(name)?;
            Filter::Exists {
                field: field(name),
                ty: None,
            }
        }
        config::Filter::And(children) => Filter::And(
            children
                .iter()
                .map(|c| build(c, generator, rng))
                .collect::<Result<_>>()?,
        ),
        config::Filter::Or(children) => Filter::Or(
            children
                .iter()
                .map(|c| build(c, generator, rng))
                .collect::<Result<_>>()?,
        ),
    })
}

fn median<T: Ord + Copy + Default>(mut values: Vec<T>) -> T {
    values.sort_unstable();
    values.get(values.len() / 2).copied().unwrap_or_default()
}

/// A leaf's `{ name = value }`: exactly one attribute.
fn single<V>(leaf: &std::collections::BTreeMap<String, V>) -> Result<(&str, &V)> {
    match leaf.iter().next() {
        Some((name, value)) if leaf.len() == 1 => Ok((name, value)),
        _ => bail!("a leaf names exactly one attribute"),
    }
}
