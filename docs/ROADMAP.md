# Roadmap

## Milestone 1: CLI Prototype

- Rust workspace. Done.
- Compact file graph. Done.
- Fallback scanner. Done.
- Aggregation. Done.
- Query/filter engine. Initial implementation done.
- CSV export. Done.
- CLI scan/export mode. Done.
- Unit tests and benchmark harness. Initial implementation done.

## Milestone 2: Windows Fast Path

- Volume discovery. Done.
- Direct NTFS volume access. Probe implemented.
- MFT record parsing. Header parser implemented.
- Hard link handling where possible.
- Fallback behavior when fast path is unavailable.
- Full MFT scan and graph construction.

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
