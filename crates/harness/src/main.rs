//! Load tests for Golem DB. Usage: golemdb-harness <scenario.toml | dir>...

mod config;
mod generate;
mod profile;
mod stats;
mod tables;
mod workloads;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use config::{Scenario, Workload};
use golemdb_api::{Api, GenesisConfig, GolemDb, MdbxOptions, OpenConfig};

pub type Db = Arc<dyn Api + Send + Sync>;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, dir] = args.as_slice()
        && flag == "--table-stats"
    {
        return tables::print(Path::new(dir));
    }
    let mut paths = Vec::new();
    for arg in &args {
        paths.extend(toml_files(Path::new(arg))?);
    }
    if paths.is_empty() {
        bail!("usage: golemdb-harness <scenario.toml | dir>...");
    }

    let scenarios = paths
        .iter()
        .map(|path| Scenario::load(path))
        .collect::<Result<Vec<_>>>()?;
    if scenarios.iter().any(|s| !s.profile_at.is_empty()) {
        profile::check()?;
    }
    let mut rows = Vec::new();
    for scenario in &scenarios {
        rows.push(run(scenario)?);
    }
    if rows.len() > 1 {
        println!(
            "{:<24} {:>8} {:>12} {:>14} {:>9}  description",
            "scenario", "ops/s", "seal p50 ms", "commit p50 ms", "disk MiB"
        );
        for row in rows {
            println!("{row}");
        }
    }
    Ok(())
}

/// Runs one scenario on a fresh database; returns its comparison-table row.
fn run(scenario: &Scenario) -> Result<String> {
    let dir = Path::new(&scenario.database.path);
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    nocow(dir)?;
    let config = OpenConfig::new(GenesisConfig {
        hash_function: scenario.database.hash,
        cell_limits: scenario.database.limits,
    });
    let options = MdbxOptions {
        max_map_size: (scenario.database.max_map_size_gib << 30) as usize,
        ..Default::default()
    };
    let db: Db = Arc::new(GolemDb::open_with_options(dir, &config, options)?);

    println!("{}", scenario.name);
    if !scenario.description.is_empty() {
        println!("  {}", scenario.description);
    }
    if scenario.preload > 0 {
        let took = workloads::preload(&db, scenario)?;
        println!(
            "  preloaded {} records in {:.1}s",
            scenario.preload,
            took.as_secs_f64()
        );
    }
    let outcome = match &scenario.workload {
        Workload::Blocks { blocks, mix } => workloads::blocks(&db, scenario, blocks, mix)?,
        Workload::Writers {
            writers,
            ops_per_branch,
            duration_secs,
            mix,
        } => workloads::writers(
            &db,
            scenario,
            *writers,
            *ops_per_branch,
            Duration::from_secs(*duration_secs),
            mix,
        )?,
    };
    drop(db);

    let stats = &outcome.stats;
    let secs = outcome.wall.as_secs_f64();
    let ops_per_sec = stats.get("ops") as f64 / secs;
    let disk_mib = dir_size(dir) as f64 / (1 << 20) as f64;
    println!(
        "  {ops_per_sec:.0} ops/s  ({} ops in {} commits, {secs:.1}s, seal and commit included)  disk {disk_mib:.0} MiB",
        stats.get("ops"),
        stats.get("commits")
    );
    for note in &outcome.notes {
        println!("  {note}");
    }
    stats.print_latency();
    println!();

    Ok(format!(
        "{:<24} {ops_per_sec:>8.0} {:>12.1} {:>14.1} {disk_mib:>9.0}  {}",
        scenario.name,
        stats.p50_ms("seal"),
        stats.p50_ms("commit"),
        scenario.description
    ))
}

/// On btrfs, turns copy-on-write off for the (empty) database directory, as
/// databases are normally run there; the files MDBX creates inherit it.
/// Other filesystems have no copy-on-write to turn off.
fn nocow(dir: &Path) -> Result<()> {
    let fs = Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(dir)
        .output()?;
    if String::from_utf8_lossy(&fs.stdout).trim() != "btrfs" {
        return Ok(());
    }
    let chattr = Command::new("chattr").arg("+C").arg(dir).output();
    match chattr {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!(
            "turning copy-on-write off for {} (btrfs): {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) => Err(e).with_context(|| format!("running chattr +C on {} (btrfs)", dir.display())),
    }
}

/// A file, or the `.toml` files directly inside a directory, sorted.
fn toml_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    files.retain(|p| p.extension().is_some_and(|ext| ext == "toml"));
    files.sort();
    Ok(files)
}

pub(crate) fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}
