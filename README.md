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

Early development. The first milestone is a CLI scanner prototype, compact data model, export path, benchmark harness, and GUI shell. The README will not claim DiskLoom is faster than WizTree until benchmark data proves it.

## License

Licensed under either of:

- Apache License, Version 2.0
- MIT License

at your option.

