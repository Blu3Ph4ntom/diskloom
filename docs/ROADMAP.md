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
- MFT record parsing. Header, fixup, file-name, data-run parsing implemented.
- Raw MFT scan and graph construction. Initial implementation done.
- Hard link handling where possible. Initial file-record count flagging done.
- Fallback behavior when fast path is unavailable. Initial CLI auto mode done.
- Extend NTFS support for attribute-list records, additional namespaces, reparse points, and richer metadata.

## Milestone 3: GUI

- Tree view sorted by size.
- File view sorted by size. Initial implementation done.
- Treemap. Initial implementation done.
- File type stats. Initial implementation done.
- Background scan worker. Initial implementation done.
- Scanner mode selection. Initial implementation done.
- Search/filter UI. Initial name, regex, extension, path, size, and allocated-size filters done.
- Shell actions. Initial Explorer, properties, recycle delete, and rename controls done.
- Dark mode. Initial implementation done.
- High-DPI polish.

## Milestone 4: Packaging and Benchmarks

- Portable release build.
- Installer support.
- Reproducible public benchmark suite.
- Screenshots and release notes.

## Later

- Snapshot and delta scans.
- Content-hash duplicate verification.
- macOS and Linux scanner implementations behind existing boundaries.
