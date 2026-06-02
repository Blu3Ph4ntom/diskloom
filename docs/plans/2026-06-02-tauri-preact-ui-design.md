# Tauri Preact App Design

DiskLoom should ship as one GUI executable: `diskloom.exe`. The root binary is the app, not a launcher for a second GUI process.

## Architecture

- Use Tauri v2 for the Windows desktop shell.
- Use Preact and Vite for the renderer.
- Keep scanning in Rust using the existing NTFS and fallback scanner crates.
- Use Tauri commands for frontend-to-Rust calls and Tauri events for scan progress.
- Keep scan state in Rust: graph, tree index, expanded nodes, selection, and cancellation flag.

## Screen

One screen only:

- top command bar with product name, path field, drive shortcuts, scanner mode, scan, and cancel
- compact status strip for scanner, elapsed time, entries, files, folders, size, and allocated size
- main virtualized file/folder tree sorted by aggregate size

No landing page, tabs, duplicate screen, treemap, secondary GUI binary, or extra windows.

## Tree Model

The frontend asks Rust for visible row slices. Rust keeps `EntryId` values, parent links, and child ranges. Rows sent to the UI contain only:

- entry id
- name
- depth
- directory/expanded flags
- child count
- total size and allocated size

Full paths remain lazy and are only reconstructed for selection or future actions.

## Elevation

Direct NTFS drive scans still request UAC when needed. The elevated process is `diskloom.exe` itself. The non-elevated instance exits after launching the elevated one, so the user does not get a separate `diskloom-ui.exe` window.

## C Drive Size Correction

Direct NTFS scans should not count reserved NTFS metadata files as normal user files. The scan tree keeps the real root record but excludes reserved metadata records and descendants under skipped metadata parents. This avoids `$BadClus` and related system files making the root appear to consume the whole volume.

## Verification

- frontend builds with `npm run build --prefix frontend`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- release `diskloom.exe` starts the GUI directly
- portable package contains the updated single GUI launcher
