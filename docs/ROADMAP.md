# Roadmap

## Milestone 1: CLI Prototype

- Rust workspace.
- Compact file graph.
- Fallback scanner.
- Aggregation.
- Query/filter engine.
- CSV export.
- CLI scan/export mode.
- Unit tests and benchmark harness.

## Milestone 2: Windows Fast Path

- Volume discovery.
- Direct NTFS volume access.
- MFT record parsing.
- Hard link handling where possible.
- Fallback behavior when fast path is unavailable.

## Milestone 3: GUI

- Tree view sorted by size.
- File view sorted by size.
- Treemap.
- File type stats.
- Background scan worker.
- Search/filter UI.
- Shell actions.
- Dark mode and high-DPI polish.

## Milestone 4: Packaging and Benchmarks

- Portable release build.
- Installer support.
- Reproducible public benchmark suite.
- Screenshots and release notes.

## Later

- Snapshot and delta scans.
- Content-hash duplicate verification.
- macOS and Linux scanner implementations behind existing boundaries.

