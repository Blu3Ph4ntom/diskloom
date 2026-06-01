# Benchmark Methodology

DiskLoom benchmark claims must be reproducible and conservative.

## Metrics

- Time to first visible result.
- Full scan time.
- Peak working set.
- Private bytes.
- UI responsiveness during scan.
- CSV export time.
- Memory use per million files.
- Behavior on 1M, 5M, and 10M file datasets.

## Comparisons

Compare against:

- WizTree.
- TreeSize.
- WinDirStat.
- Windows Explorer search or traversal behavior where relevant.

Use [WizTree Public Claims Baseline](benchmarks/wiztree-public-claims.md) to map vendor-published WizTree claims to DiskLoom benchmark runs.

## Rules

- Run release builds.
- Record hardware, Windows version, filesystem, drive type, and dataset shape.
- Repeat runs and report median and range.
- Separate cold-cache and warm-cache results.
- Avoid claims that are not backed by published data.
- Publish commands, dataset generator settings, raw output, and analysis scripts.

## Current Harness

Repeated fallback scan timing:

```powershell
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --scanner fallback
```

Benchmark scanner modes:

```powershell
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --scanner auto
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --scanner ntfs
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --scanner fallback
```

Synthetic dataset generation:

```powershell
cargo run --release -p diskloom-bench -- dataset D:\diskloom-bench --dirs 1000 --files-per-dir 1000 --bytes-per-file 0
```

The harness emits CSV rows for scanner mode, fallback behavior, elapsed time, entry counts, sampled peak working set, sampled peak private bytes, final working set, final private bytes, and peak private bytes per million entries. Memory sampling is in-process and interval-based, so published runs must include the `--sample-ms` value. UI responsiveness and competitor automation still need dedicated collectors before public claims are made.
