#!/usr/bin/env python3
"""Flamegraph SVG of one profiled commit (see `profile_at` in the README).

    scripts/flamegraph.py results/profiles/large-realistic-commit250.perf.data results/large-realistic.csv 250

Needs perf and inferno (`cargo install inferno`). Every build has frame pointers
(.cargo/config.toml), so `cargo load` recordings unwind.

Two adjustments to the raw profile, so widths add up to the commit's real time:
- libmdbx runs MDBX's commit (page writes, fsync) on its own thread; those samples
  are moved under `commit`, marked [libmdbx writer thread].
- Time a call spent off the CPU (disk, other kernel threads) is not sampled; it is
  added as a [waiting] bar: wall time from the commit log minus sampled CPU time.
"""

import csv
import re
import subprocess
import sys
from collections import Counter

HZ = 999  # the -F in profile.rs; the cpu-clock event counts nanoseconds
RUN = "harness;golemdb_harness::run;golemdb_harness::workloads::blocks;golemdb_harness::workloads::run_branch"


def clean(frames):
    out = []
    for f in frames:
        if "core::ops::try_trait::Try>::branch" in f:
            continue
        out.append(re.sub(r"^<golemdb_api::database::Engine<.*> as golemdb_api::Api>::", "golemdb_api::Api::", f))
    return out


def main(data, commit_log, commit, svg=None):
    svg = svg or data.removesuffix(".perf.data") + ".svg"
    script = subprocess.run(["perf", "script", "-i", data], capture_output=True, check=True).stdout
    collapsed = subprocess.run(["inferno-collapse-perf"], input=script, capture_output=True, check=True).stdout.decode()

    stacks, cpu = Counter(), Counter()
    for line in collapsed.splitlines():
        stack, n = line.rsplit(" ", 1)
        frames, ms = stack.split(";"), int(n) / 1e6
        if "golemdb_harness::main" in frames:
            path = ["harness"] + clean(frames[frames.index("golemdb_harness::main") + 1 :])
        else:
            spawn = [i for i, f in enumerate(frames) if "libmdbx::database::NoWriteMap>>::open_with_options" in f]
            if not spawn:
                path = ["[other threads]"] + frames[1:]
            else:
                rest = frames[spawn[-1] + 1 :]
                start = next((i for i, f in enumerate(rest) if "mdbx_txn" in f), 0)
                call = "begin" if any("mdbx_txn_begin" in f for f in rest) else "commit"
                path = RUN.split(";") + [f"golemdb_api::Api::{call}", "[libmdbx writer thread]"] + clean(rest[start:])
        for call in ("seal", "commit"):
            if f"golemdb_api::Api::{call}" in path:
                cpu[call] += ms
        stacks[";".join(path)] += ms

    row = next(r for r in csv.DictReader(open(commit_log)) if r["commit"] == str(commit))
    for call in ("seal", "commit"):
        waiting = float(row[f"{call}_ms"]) - cpu[call]
        if waiting > 0.02 * float(row["branch_ms"]):
            stacks[f"{RUN};golemdb_api::Api::{call};[waiting: off CPU, wall time minus sampled CPU]"] += waiting

    folded = "".join(f"{k} {round(v * 1000)}\n" for k, v in stacks.items())  # microseconds
    title = f"Commit {commit}: {row['ops']} writes, database {float(row['db_mib']):,.0f} MiB after it"
    sub = f"seal {float(row['seal_ms']):,.0f} ms, commit {float(row['commit_ms']):,.0f} ms. Widths in ms; perf at {HZ} Hz, both threads."
    out = subprocess.run(
        ["inferno-flamegraph", "--title", title, "--subtitle", sub, "--countname", "ms", "--factor", "0.001",
         "--minwidth", "0.2", "--width", "1600"],
        input=folded.encode(), capture_output=True, check=True,
    ).stdout
    open(svg, "wb").write(out)
    print(svg)


if __name__ == "__main__":
    main(*sys.argv[1:])
