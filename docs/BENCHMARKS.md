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
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner fallback
```

Benchmark scanner modes:

```powershell
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner auto
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner ntfs
cargo run --release -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner fallback
```

Summarize a captured CSV run:

```powershell
target\release\diskloom-bench.exe scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner ntfs > target\bench-ntfs.csv
target\release\diskloom-bench.exe summarize target\bench-ntfs.csv
```

Measure CSV export time after scanning:

```powershell
target\release\diskloom-bench.exe export C:\ --iterations 5 --sample-ms 10 --scanner ntfs > target\bench-export-ntfs.csv
target\release\diskloom-bench.exe export C:\ --iterations 5 --sample-ms 10 --scanner fallback --output-dir target\exports > target\bench-export-fallback.csv
```

Compare a captured run to a source-labeled WizTree public claim:

```powershell
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-ssd-460gb
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-hdd-25gb
```

Run a local suite that writes raw CSVs, summaries, public-claim reference rows, and a Markdown report:

```powershell
target\release\diskloom-bench.exe suite C:\ target\bench-suite --iterations 5 --sample-ms 10 --progress-every 1024 --scanner ntfs
target\release\diskloom-bench.exe suite C:\ target\bench-suite-fallback --iterations 5 --sample-ms 10 --progress-every 1024 --scanner fallback --claim wiztree-ssd-460gb
```

Synthetic dataset generation:

```powershell
cargo run --release -p diskloom-bench -- dataset D:\diskloom-bench --dirs 1000 --files-per-dir 1000 --bytes-per-file 0
```

The harness emits CSV rows for scanner mode, fallback behavior, elapsed time, first-result time, entry counts, sampled peak working set, sampled peak private bytes, final working set, final private bytes, and peak private bytes per million entries. The `scan` command uses `--progress-every` to enable fallback progress callbacks for first-result timing; non-streaming scanner paths report first-result time as full elapsed time. The `export` command reports scan elapsed time, CSV export elapsed time, total elapsed time, and exported byte count, writing the exported CSV to an in-memory sink by default or to `--output-dir` when disk output should be included. The `summarize` command computes run count, scanner set, fallback count, elapsed median/range, first-result median/range, and peak memory maxima from captured CSV rows. The `compare-public` command emits a source-labeled reference comparison against WizTree's historical published claims and marks it as `reference_only_vendor_claim_not_same_machine`. The `suite` command runs scan and export measurements, writes `scan.csv`, `scan-summary.csv`, `export.csv`, `public-comparison.csv`, and `report.md`, and defaults to all source-backed public WizTree claims when no `--claim` is supplied. Memory sampling is in-process and interval-based, so published runs must include the `--sample-ms` value. UI responsiveness and competitor automation still need dedicated collectors before public claims are made.
