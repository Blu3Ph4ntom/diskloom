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
- Compact graph with string interning, integer IDs, parent IDs, aggregation, and lazy path reconstruction.
- Non-admin fallback scanner with Windows allocated-size reporting.
- CLI scan/export mode.
- Windows volume discovery.
- Direct NTFS MFT scanner using raw volume access, MFT data-run parsing, and file-record enumeration.
- MFT file-record, fixup, `$FILE_NAME`, and `$DATA` runlist parser tests.
- CSV export.
- Duplicate candidate grouping by size/name/date.
- File type statistics and initial treemap layout.
- egui desktop shell with background scans, live fallback progress counts, scanner mode selection, query-backed file filters, file actions, files, types, and treemap tabs.
- Benchmark harness for repeated scan timing, sampled process memory, and synthetic dataset creation.

The direct NTFS scanner is an early fast path. It can require elevated access to open raw volumes, and it currently focuses on primary file records with resident names and non-resident data runs. DiskLoom falls back to directory traversal when raw volume access is unavailable. This README will not claim DiskLoom is faster than WizTree until benchmark data proves it.

## Usage

Run a fallback scan:

```powershell
cargo run -p diskloom-cli -- scan . --scanner fallback --limit 25
```

Run auto mode, which tries direct NTFS MFT scanning for drive roots and falls back if needed:

```powershell
cargo run -p diskloom-cli -- scan C:\ --scanner auto --limit 25
```

Force direct NTFS MFT scanning:

```powershell
cargo run -p diskloom-cli -- scan C:\ --scanner ntfs --limit 25
```

Export CSV:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --csv target/users.csv
```

Show file type statistics:

```powershell
cargo run -p diskloom-cli -- scan C:\Users --scanner fallback --file-types
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
cargo run -p diskloom-ui
```

Run the benchmark harness:

```powershell
cargo run -p diskloom-bench -- scan . --iterations 5 --sample-ms 10 --scanner fallback
```

See [benchmark methodology](docs/BENCHMARKS.md) and [WizTree public claims baseline](docs/benchmarks/wiztree-public-claims.md) before making performance claims.

Create a synthetic dataset:

```powershell
cargo run -p diskloom-bench -- dataset target/bench-tree --dirs 100 --files-per-dir 100 --bytes-per-file 0
```

Build portable binaries:

```powershell
cargo build --release -p diskloom-cli -p diskloom-ui
```

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.
