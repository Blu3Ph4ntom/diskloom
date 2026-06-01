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
- Record hardware, Windows version, filesystem, drive type, dataset label, dataset shape, and cache state.
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
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-ssd-460gb
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-hdd-25gb
target\release\diskloom-bench.exe compare-public target\bench-ntfs.csv --claim wiztree-ssd-500gb-typical
```

Compare a captured DiskLoom run to same-machine competitor measurements:

```powershell
target\release\diskloom-bench.exe compare-competitor target\bench-ntfs.csv target\competitors.csv --dataset-label workstation-c --cache-state warm
```

Competitor CSV input uses one row per measured run:

```csv
tool,version,dataset_label,cache_state,scanner_scope,elapsed_ms,peak_private_bytes,notes
WizTree,4.25,workstation-c,warm,ntfs_mft,5230,,
TreeSize,9.0,workstation-c,warm,traversal,18000,,
```

Run a local suite that writes raw CSVs, summaries, public-claim reference rows, an audit CSV, a machine-readable manifest, and a Markdown report:

```powershell
target\release\diskloom-bench.exe suite C:\ target\bench-suite --dataset-label workstation-c --cache-state warm --iterations 5 --sample-ms 10 --progress-every 1024 --scanner ntfs
target\release\diskloom-bench.exe suite C:\ target\bench-suite-fallback --dataset-label workstation-c --cache-state cold-after-reboot --iterations 5 --sample-ms 10 --progress-every 1024 --scanner fallback --claim wiztree-ssd-460gb
.\scripts\run-bench-suite.ps1 -Path C:\ -Scanner ntfs -Iterations 5 -DatasetLabel workstation-c -CacheState warm
.\scripts\run-bench-suite.ps1 -Path C:\ -Scanner fallback -Iterations 5 -DatasetLabel workstation-c -CacheState warm -Claim wiztree-ssd-500gb-typical
```

Synthetic dataset generation:

```powershell
cargo run --release -p diskloom-bench -- dataset D:\diskloom-bench --dirs 1000 --files-per-dir 1000 --bytes-per-file 0
```

The harness emits CSV rows for scanner mode, fallback behavior, elapsed time, first-result time, entry counts, sampled peak working set, sampled peak private bytes, final working set, final private bytes, and peak private bytes per million entries. The `scan` command uses `--progress-every` to enable fallback progress callbacks for first-result timing; non-streaming scanner paths report first-result time as full elapsed time. The `export` command reports scan elapsed time, CSV export elapsed time, total elapsed time, and exported byte count, writing the exported CSV to an in-memory sink by default or to `--output-dir` when disk output should be included. The `summarize` command computes run count, scanner set, fallback count, elapsed median/range, first-result median/range, and peak memory maxima from captured CSV rows. The `compare-public` command emits source-labeled reference comparisons against WizTree's published exact and ranged public claims, records claim minimum and maximum milliseconds, compares DiskLoom's median against both ends of the range, defaults to all registered claims when no `--claim` is supplied, and marks every row as `reference_only_vendor_claim_not_same_machine`. Public comparison rows also include `claim_scan_scope`, `comparison_applicability`, and `diskloom_median_position`; current WizTree timing claims are scoped to `ntfs_mft`, so fallback DiskLoom runs are explicitly marked `not_aligned_requires_ntfs_mft`, while median position only says whether the DiskLoom median is below, within, or above the public range. The `compare-competitor` command ingests manually recorded same-machine competitor CSV rows, groups them by tool/version/dataset/cache/scope, compares each competitor median against DiskLoom's median, and labels context mismatches instead of treating them as valid local proof. The `suite` command runs scan and export measurements, writes `scan.csv`, `scan-summary.csv`, `export.csv`, `public-comparison.csv`, `audit.csv`, `metadata.txt`, `manifest.json`, and `report.md`, records `--dataset-label` and `--cache-state`, and defaults to all source-backed public WizTree claims when no `--claim` is supplied. `audit.csv` flags missing context, too few iterations, dirty Git state, reference-only public claims, and scanner-scope mismatches before results are used publicly. `manifest.json` uses the `diskloom.benchmark-suite.v1` schema and captures run settings, Git state, environment fields, summary metrics, audit rows, public-claim references, and artifact names for machine ingestion. The `scripts/run-bench-suite.ps1` wrapper builds the release benchmark binary, creates a timestamped suite directory under `target\bench-suites`, forwards `-DatasetLabel` and `-CacheState`, runs the suite, and prints the report, audit, and public-comparison paths. `metadata.txt` includes command settings, dataset label, cache state, the exact benchmark command line, Git revision, Git dirty state, best-effort detected Windows environment fields, logical CPU count, physical memory, and blank publication-checklist fields that must be filled before publishing benchmark claims. Memory sampling is in-process and interval-based, so published runs must include the `--sample-ms` value. UI responsiveness and competitor automation still need dedicated collectors before public claims are made.
