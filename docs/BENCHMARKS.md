# Benchmark Methodology

DiskLoom benchmarks measure the app and CLI on reproducible local datasets. Results should include hardware, Windows version, drive type, cache state, scanner mode, and dataset shape.

## Metrics

- Time to first result.
- Full scan time.
- Peak private bytes.
- Peak working set.
- Memory per million entries.
- UI responsiveness during scan.
- CSV export time.
- Repeated scan behavior.

## Scanner Modes

- `ntfs`: direct NTFS metadata scan for supported Windows volumes.
- `fallback`: normal directory traversal.
- `auto`: direct NTFS when available, fallback otherwise.

## CLI Examples

```powershell
cargo run -p diskloom-bench -- scan . --iterations 5 --sample-ms 10 --scanner fallback --output target\bench.csv
cargo run -p diskloom-bench -- export . --iterations 5 --sample-ms 10 --scanner fallback --output target\export-bench.csv
cargo run -p diskloom-bench -- responsiveness . --iterations 5 --tick-ms 16 --scanner fallback --output target\responsiveness-bench.csv
cargo run -p diskloom-bench -- summarize target\bench.csv
```

Create a synthetic dataset:

```powershell
cargo run -p diskloom-bench -- dataset target\bench-tree --dirs 100 --files-per-dir 100 --bytes-per-file 0
```

Run a suite:

```powershell
cargo run -p diskloom-bench -- suite . target\bench-suite --dataset-label repo-smoke --cache-state warm --hardware-label workstation-a --dataset-shape repo-tree --iterations 5 --sample-ms 10 --ui-tick-ms 16 --scanner fallback
.\scripts\run-bench-suite.ps1 -Path . -Scanner fallback -Iterations 5 -DatasetLabel repo-smoke -CacheState warm -HardwareLabel workstation-a -DatasetShape repo-tree -UiTickMs 16
```

## Reporting Rules

When publishing results, include:

- DiskLoom version or commit hash.
- Windows version.
- CPU and memory.
- Drive model and file system.
- Dataset size and file count.
- Cache state, cold or warm.
- Scanner mode.
- Raw CSV output.

Do not publish vague speed claims without the raw benchmark data needed to reproduce them.
