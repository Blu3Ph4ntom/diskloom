# DiskLoom

See your disk clearly.

DiskLoom is a Rust-native Windows disk space analyzer built to challenge WizTree with extreme scan speed, low memory use, responsive interaction, and an auditable open-source codebase.

The project is Windows-first for v1. Future macOS and Linux support may arrive through scanner boundaries, but those platforms do not shape the first release.

## Goals

- Direct NTFS MFT scanning fast path.
- Non-admin fallback directory scanner.
- Compact file graph designed for millions of files.
- Streaming results so the UI stays responsive during large scans.
- Portable executable support.
- No telemetry.
- No background service.
- Honest benchmark claims backed by reproducible measurements.

## V1 Target

- Scan NTFS drives with the direct MFT path.
- Scan folders and non-NTFS locations with fallback traversal.
- Tree view and file view sorted by size.
- Treemap and file type statistics.
- Size and allocated size columns.
- Search and filtering by name, extension, size, allocated size, modified date, and path.
- Duplicate candidates by size/name/date, with content hashing staged later.
- CSV export and CLI export mode.
- Windows shell actions: open in Explorer, properties, recycle delete, and rename.
- Dark mode and high-DPI support.

## Status

Early development. Implemented pieces include:

- Rust workspace with separate core, scan, query, duplicate, export, Windows, NTFS, CLI, UI, and benchmark crates.
- Root launcher so `diskloom.exe` opens the GUI by default and routes `diskloom.exe cli ...` to the CLI.
- Compact graph with string interning, integer IDs, parent IDs, aggregation, and lazy path reconstruction.
- Non-admin fallback scanner with Windows allocated-size reporting.
- CLI scan/export mode.
- Windows volume discovery.
- Direct NTFS MFT scanner using raw volume access, MFT data-run parsing, and file-record enumeration.
- MFT file-record, fixup, `$FILE_NAME`, and `$DATA` runlist parser tests.
- CSV export.
- Duplicate candidate grouping by size/name/date.
- File type statistics and initial treemap layout.
- Native Rust Win32 desktop shell with a scan control rail, discovered drive shortcuts, background scans, live progress counts, scanner mode selection, cancellation, and tree, files, types, and treemap tabs.
- Query filters, CSV export, duplicate grouping, and Windows file actions remain implemented in shared crates and CLI paths; they are being rewired into the native GUI shell after the egui retirement.
- Benchmark harness for repeated scan timing, sampled process memory, foreground tick-gap responsiveness, synthetic dataset creation, same-machine competitor comparisons, suite manifests, and audit outputs.

The direct NTFS scanner is an early fast path. It can require elevated access to open raw volumes, and it currently focuses on primary file records with resident names and non-resident data runs. DiskLoom falls back to directory traversal when raw volume access is unavailable. This README will not claim DiskLoom is faster than WizTree until benchmark data proves it.

## Usage

Run a fallback scan:

```powershell
cargo run -p diskloom-cli -- scan . --scanner fallback --limit 25
```

Run auto mode, which uses direct NTFS MFT scanning for drive roots and fallback traversal for folders:

```powershell
cargo run -p diskloom-cli -- scan C:\ --scanner auto --limit 25
```

Force direct NTFS MFT scanning:

```powershell
cargo run -p diskloom-cli -- scan C:\ --scanner ntfs --limit 25
```

Direct NTFS drive scans require administrator access on Windows. If DiskLoom is not already elevated, the CLI, GUI, and benchmark harness request UAC elevation and relaunch the same command instead of silently falling back to traversal.

Export CSV:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --csv target/users.csv
```

Show file type statistics:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --scanner fallback --file-types
```

Filter by full path:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --scanner fallback --path AppData --limit 25
```

Show duplicate candidates:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --scanner fallback --duplicates --limit 25
```

List Windows volumes:

```powershell
cargo run -p diskloom-cli -- volumes
```

Probe the NTFS fast-path boundary:

```powershell
cargo run -p diskloom-cli -- ntfs-probe C:
```

Launch the GUI shell:

```powershell
cargo run
```

The root launcher builds missing or stale release binaries if needed and then starts the GUI. Use `cargo run -- scan . --scanner fallback --limit 25` for the CLI, `cargo run -- cli scan . --scanner fallback --limit 25` for explicit CLI routing, or `cargo run -- bench --help` for the benchmark harness.

Run the benchmark harness:

```powershell
cargo run -p diskloom-bench -- scan . --iterations 5 --sample-ms 10 --progress-every 1024 --scanner fallback > target/bench.csv
cargo run -p diskloom-bench -- scan C:\ --iterations 5 --sample-ms 10 --progress-every 1024 --scanner ntfs --output target\bench-ntfs.csv
cargo run -p diskloom-bench -- export . --iterations 5 --sample-ms 10 --scanner fallback > target/export-bench.csv
cargo run -p diskloom-bench -- responsiveness . --iterations 5 --tick-ms 16 --progress-every 1024 --scanner fallback > target/responsiveness-bench.csv
cargo run -p diskloom-bench -- summarize target\bench.csv
cargo run -p diskloom-bench -- compare-public target\bench.csv --claim wiztree-ssd-460gb
cargo run -p diskloom-bench -- competitor-template --examples > target\competitors.csv
cargo run -p diskloom-bench -- compare-competitor target\bench.csv target\competitors.csv --dataset-label repo-smoke --cache-state warm
cargo run -p diskloom-bench -- suite . target\bench-suite --dataset-label repo-smoke --cache-state warm --hardware-label workstation-a --dataset-shape repo-tree --iterations 5 --sample-ms 10 --ui-tick-ms 16 --scanner fallback --competitor-csv target\competitors.csv
.\scripts\run-bench-suite.ps1 -Path . -Scanner fallback -Iterations 5 -DatasetLabel repo-smoke -CacheState warm -HardwareLabel workstation-a -DatasetShape repo-tree -UiTickMs 16 -CompetitorCsv target\competitors.csv
```

See [benchmark methodology](docs/BENCHMARKS.md) and [WizTree public claims baseline](docs/benchmarks/wiztree-public-claims.md) before making performance claims.

For direct NTFS drive benchmarks from a non-elevated shell, prefer `--output` over shell redirection. DiskLoom requests UAC, waits for the elevated process, and writes the CSV from that elevated process.

Create a synthetic dataset:

```powershell
cargo run -p diskloom-bench -- dataset target/bench-tree --dirs 100 --files-per-dir 100 --bytes-per-file 0
```

Build a portable Windows zip:

```powershell
.\scripts\package-portable.ps1
```

The portable package includes:

- `diskloom.exe`: product launcher. Double-click starts the GUI; `diskloom.exe scan ...` or `diskloom.exe cli ...` runs CLI commands.
- `diskloom-cli.exe`: direct CLI entry for scripts.
- `diskloom-ui.exe`: direct GUI entry.
- `diskloom-bench.exe`: benchmark harness.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
