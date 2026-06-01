# DiskLoom Design

## Product

DiskLoom is a Rust-native Windows disk space analyzer.

Tagline: See your disk clearly.

The first release is Windows-only and targets fast NTFS scanning, low memory use, responsive interaction, portable executable distribution, no telemetry, and no background service.

## Positioning

DiskLoom is built to challenge WizTree. It should not claim to be faster until reproducible benchmarks prove scan time, memory use, and responsiveness advantages.

## Architecture

DiskLoom uses a Rust workspace with scanner logic independent from UI code.

- `diskloom-core`: compact file graph, string interning, aggregation, path reconstruction, and shared data types.
- `diskloom-windows`: Windows volume discovery and shell/file operations.
- `diskloom-ntfs`: direct NTFS/MFT scanner fast path.
- `diskloom-scan`: fallback directory traversal and scanner traits.
- `diskloom-query`: filtering, sorting, and search.
- `diskloom-dupes`: duplicate candidate grouping.
- `diskloom-export`: CSV export and later snapshot import/export.
- `diskloom-cli`: CLI scan and export mode.
- `diskloom-ui`: responsive desktop UI.
- `diskloom-bench`: benchmark harness.

The core graph stores integer IDs, parent IDs, interned names, packed metadata arrays, and lazy path reconstruction. It avoids full path strings per entry and avoids heap-heavy per-file objects.

## Data Flow

Scanners stream discovered entries into the core builder. The builder interns names, stores compact metadata, and maintains parent-child relationships. Aggregation computes recursive size and allocated size per directory after scan batches land. Query and export layers read from immutable snapshots so UI work does not block scanner ingestion.

The UI consumes progress updates and snapshot batches from background workers. The main UI thread never performs deep traversal or filesystem scanning.

## Scanner Strategy

The fallback scanner uses standard directory traversal and works without administrator privileges. The NTFS scanner is Windows-only and isolates direct volume access, record parsing, and unsafe code in the NTFS crate. If the fast path cannot open a volume or validate NTFS metadata, DiskLoom falls back to traversal and reports the mode honestly.

## UI Choice

The v1 UI uses `egui` through `eframe`. This keeps the app Rust-native, portable-executable friendly, high-DPI capable, and quick to iterate. A richer Windows-native UI can be reconsidered after scanner correctness and benchmark discipline are established.

## Error Handling

Library crates use typed errors with `thiserror`. Binaries use `anyhow` for human-readable context. Unsafe code is kept inside small Windows/NTFS modules and documented with safety rationale.

## Testing

Tests cover string interning, graph aggregation, path reconstruction, filters, duplicate grouping, CSV escaping, and scanner behavior on synthetic temporary trees.

Benchmarks cover time to first result, full scan time, peak memory, export time, and behavior on generated 1M, 5M, and 10M file datasets where feasible.

