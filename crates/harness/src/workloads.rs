use std::collections::HashSet;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use golemdb_api::{ApiError, PatchInput, RecordInput, RecordKey};
use rand::Rng;

use crate::Db;
use crate::config::{Blocks, Mix, Scenario};
use crate::generate::{Generator, key};
use crate::stats::Stats;

pub struct Outcome {
    pub stats: Stats,
    pub wall: Duration,
    /// Workload-specific result lines.
    pub notes: Vec<String>,
}

enum Op {
    Create(RecordKey, RecordInput),
    Patch(RecordKey, PatchInput),
    Delete(RecordKey),
}

/// Seal and commit time of one committed branch.
struct Timing {
    seal: Duration,
    commit: Duration,
}

/// begin → ops → seal → commit, timing each call. Returns `None` if another
/// branch committed first (first-committer-wins); that is not an error.
fn run_branch(db: &Db, ops: &[Op], stats: &mut Stats) -> Result<Option<Timing>> {
    let branch = stats.time("begin", || db.begin())?;
    let lost = |stats: &mut Stats| -> Result<Option<Timing>> {
        stats.count("lost", 1);
        let _ = db.discard(branch);
        Ok(None)
    };
    for op in ops {
        // Clone outside the timer: only the engine call is measured.
        let result = match op {
            Op::Create(key, input) => {
                let input = input.clone();
                stats.time("create", || db.create(branch, *key, input).map(drop))
            }
            Op::Patch(key, input) => {
                let input = input.clone();
                stats.time("patch", || db.patch(branch, *key, input))
            }
            Op::Delete(key) => stats.time("delete", || db.delete(branch, *key)),
        };
        match result {
            Ok(()) => {}
            Err(ApiError::HandleInvalid | ApiError::Conflict) => return lost(stats),
            Err(e) => return Err(e.into()),
        }
    }
    let start = Instant::now();
    let sealed = db.seal(branch);
    let seal = start.elapsed();
    stats.record("seal", seal);
    match sealed {
        Ok(_) => {}
        Err(ApiError::HandleInvalid | ApiError::Conflict) => return lost(stats),
        Err(e) => return Err(e.into()),
    }
    // A losing commit waits behind the winner, so it is timed separately.
    let start = Instant::now();
    let commit = match db.commit(branch) {
        Ok(_) => {
            let commit = start.elapsed();
            stats.record("commit", commit);
            commit
        }
        Err(ApiError::HandleInvalid | ApiError::Conflict) => {
            stats.record("commit lost", start.elapsed());
            return lost(stats);
        }
        Err(e) => return Err(e.into()),
    };
    stats.count("commits", 1);
    stats.count("ops", ops.len() as u64);
    Ok(Some(Timing { seal, commit }))
}

/// Keys one writer has committed, so it can patch and delete its own records.
struct Pool {
    seed: u64,
    stream: u64,
    next: u64,
    live: Vec<RecordKey>,
}

struct Plan {
    ops: Vec<Op>,
    created: Vec<RecordKey>,
    deleted: Vec<usize>,
}

impl Pool {
    fn new(seed: u64, stream: u64) -> Self {
        Self {
            seed,
            stream,
            next: 0,
            live: Vec::new(),
        }
    }

    fn plan(&mut self, generator: &mut Generator, n: usize, mix: &Mix) -> Result<Plan> {
        let total = mix.create + mix.patch + mix.delete;
        let mut used = HashSet::new();
        let mut plan = Plan {
            ops: Vec::with_capacity(n),
            created: Vec::new(),
            deleted: Vec::new(),
        };
        for _ in 0..n {
            let roll = generator.rng().random_range(0..total);
            // Patch/delete need a committed key not already used in this branch.
            let target = (roll >= mix.create && !self.live.is_empty())
                .then(|| generator.rng().random_range(0..self.live.len()))
                .filter(|i| used.insert(*i));
            plan.ops.push(match target {
                Some(i) if roll < mix.create + mix.patch => {
                    Op::Patch(self.live[i], generator.patch()?)
                }
                Some(i) => {
                    plan.deleted.push(i);
                    Op::Delete(self.live[i])
                }
                None => {
                    let k = key(self.seed, self.stream, self.next);
                    self.next += 1;
                    plan.created.push(k);
                    Op::Create(k, generator.record()?)
                }
            });
        }
        Ok(plan)
    }

    fn committed(&mut self, plan: Plan) {
        let mut deleted = plan.deleted;
        deleted.sort_unstable_by(|a, b| b.cmp(a)); // descending keeps swap_remove indexes valid
        for i in deleted {
            self.live.swap_remove(i);
        }
        self.live.extend(plan.created);
    }
}

/// Fills the database before measurement. Uses its own key stream.
pub fn preload(db: &Db, scenario: &Scenario) -> Result<Duration> {
    let start = Instant::now();
    let mut generator = Generator::new(&scenario.shape, scenario.seed, u64::MAX)?;
    let mut pool = Pool::new(scenario.seed, u64::MAX);
    let mut done = 0;
    while done < scenario.preload {
        let n = 10_000.min(scenario.preload - done) as usize;
        let plan = pool.plan(&mut generator, n, &Mix::default())?;
        if run_branch(db, &plan.ops, &mut Stats::default())?.is_none() {
            bail!("preload lost a commit race");
        }
        done += n as u64;
    }
    Ok(start.elapsed())
}

pub fn blocks(db: &Db, scenario: &Scenario, blocks: &[Blocks], mix: &Mix) -> Result<Outcome> {
    let mut stats = Stats::default();
    let mut generator = Generator::new(&scenario.shape, scenario.seed, 0)?;
    let mut pool = Pool::new(scenario.seed, 0);
    let mut notes = Vec::new();
    // Only time inside branches counts; generating records is harness work.
    let mut wall = Duration::ZERO;
    let mut log = match &scenario.commit_log {
        Some(path) => {
            let mut file = std::io::BufWriter::new(std::fs::File::create(path)?);
            writeln!(file, "commit,ops,seal_ms,commit_ms,branch_ms,db_mib")?;
            Some(file)
        }
        None => None,
    };
    let db_dir = std::path::Path::new(&scenario.database.path);
    let mut commit_no = 0;
    for spec in blocks {
        let mix = spec.mix.as_ref().unwrap_or(mix);
        if spec.warmup {
            for _ in 0..spec.repeat {
                let plan = pool.plan(&mut generator, spec.ops, mix)?;
                if run_branch(db, &plan.ops, &mut Stats::default())?.is_none() {
                    bail!("a single writer lost a commit race");
                }
                pool.committed(plan);
            }
            continue;
        }
        let mut rates = Vec::new();
        let mut size_time = Duration::ZERO;
        for _ in 0..spec.repeat {
            let plan = pool.plan(&mut generator, spec.ops, mix)?;
            let block_start = Instant::now();
            let Some(timing) = run_branch(db, &plan.ops, &mut stats)? else {
                bail!("a single writer lost a commit race");
            };
            let took = block_start.elapsed();
            commit_no += 1;
            if let Some(file) = &mut log {
                let ms = |d: Duration| d.as_secs_f64() * 1000.0;
                writeln!(
                    file,
                    "{commit_no},{},{:.3},{:.3},{:.3},{:.1}",
                    spec.ops,
                    ms(timing.seal),
                    ms(timing.commit),
                    ms(took),
                    crate::dir_size(db_dir) as f64 / (1 << 20) as f64
                )?;
            }
            wall += took;
            size_time += took;
            rates.push(spec.ops as f64 / took.as_secs_f64());
            pool.committed(plan);
        }
        // Total ops over total time: averaging per-block rates would overweight fast blocks.
        let rate = (spec.ops * spec.repeat) as f64 / size_time.as_secs_f64();
        // Medians over the first and last 10% of blocks: a single block is too noisy
        // (the first commit after a large one is often unusually fast).
        let tenth = rates.len().div_ceil(10);
        notes.push(format!(
            "blocks of {:>6}: {rate:>7.0} ops/s  (first 10% of blocks {:.0}, last 10% {:.0})",
            spec.ops,
            median(&rates[..tenth]),
            median(&rates[rates.len() - tenth..])
        ));
    }
    Ok(Outcome { stats, wall, notes })
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

pub fn writers(
    db: &Db,
    scenario: &Scenario,
    writers: usize,
    ops_per_branch: usize,
    duration: Duration,
    mix: &Mix,
) -> Result<Outcome> {
    let start = Instant::now();
    let failed = AtomicBool::new(false);
    let merged = Mutex::new(Stats::default());
    std::thread::scope(|scope| -> Result<()> {
        let threads: Vec<_> = (0..writers as u64)
            .map(|stream| {
                let (failed, merged) = (&failed, &merged);
                scope.spawn(move || -> Result<()> {
                    let mut stats = Stats::default();
                    let mut generator = Generator::new(&scenario.shape, scenario.seed, stream)?;
                    let mut pool = Pool::new(scenario.seed, stream);
                    let running = || start.elapsed() < duration && !failed.load(Ordering::Relaxed);
                    while running() {
                        let plan = pool.plan(&mut generator, ops_per_branch, mix)?;
                        // A lost race re-executes the same batch on the new head.
                        loop {
                            match run_branch(db, &plan.ops, &mut stats) {
                                Ok(Some(_)) => {
                                    pool.committed(plan);
                                    break;
                                }
                                Ok(None) if running() => continue,
                                Ok(None) => break,
                                Err(e) => {
                                    failed.store(true, Ordering::Relaxed);
                                    return Err(e);
                                }
                            }
                        }
                    }
                    merged.lock().expect("lock").merge(stats);
                    Ok(())
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("writer panicked")?;
        }
        Ok(())
    })?;
    let stats = merged.into_inner().expect("lock");
    let attempts = stats.get("commits") + stats.get("lost");
    let notes = vec![format!(
        "lost races: {} of {attempts} branch attempts ({:.0}% of work thrown away)",
        stats.get("lost"),
        100.0 * stats.get("lost") as f64 / attempts.max(1) as f64
    )];
    Ok(Outcome {
        stats,
        wall: start.elapsed(),
        notes,
    })
}
