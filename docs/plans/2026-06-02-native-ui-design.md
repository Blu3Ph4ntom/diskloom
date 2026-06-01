# Native Windows UI Design

DiskLoom v1 should use a Rust-owned Windows desktop shell instead of a retained cross-platform widget layer. The GUI remains Windows-only for v1 and should optimize for scan visibility, low memory, and predictable operation on drives with millions of entries.

## Approach

Use Rust with Win32, Direct2D, and DirectWrite through the `windows` crate. Do not introduce C++, .NET, WebView, Electron, or Tauri. Keep all scanner, query, duplicate, export, and shell-action logic in existing crates. The UI crate owns only presentation state, input routing, rendering, and background job coordination.

The first native milestone is a real Win32 application window with a polished static shell and lightweight scan launch. Direct2D/DirectWrite rendering should follow immediately after the Win32 scaffold is stable. The old egui shell is retired rather than expanded.

## Product Shape

DiskLoom is a dense analysis tool, not a marketing surface. The first screen should show the working app:

- Left scan rail with path, drive shortcuts, scanner mode, status, and export controls.
- Main content area with compact tabs for Tree, Files, Types, Treemap, and Duplicates.
- Data-first typography using Segoe UI and monospaced numerics where columns need alignment.
- Restrained dark theme with tinted neutral surfaces and one quiet accent for active state and primary actions.
- No decorative cards, gradients, hero layouts, or instructional copy.

## Memory Model

The UI must not duplicate the file graph. It receives an `Arc<FileGraph>` snapshot from the scanner and stores compact view state:

- selected `EntryId`
- active tab
- filter text and parsed query
- visible row ranges
- small sorted/top-N ID buffers
- cached display strings only for visible rows

Full paths are reconstructed on demand. Duplicate groups and expensive filtered views are computed lazily and cancellably. The renderer must virtualize large lists and avoid keeping millions of row widgets, labels, or full-path strings alive.

## Rendering

Use a fixed application layout:

- top command/status strip
- left rail width around 300 px, collapsible later
- main table area with fixed row height
- status/footer line for scan progress and memory-sensitive counts

Rows render from IDs directly. Tables use column positions, clipping, and visible-row calculation from scroll offset. Treemap rendering operates from a bounded set of largest nodes first, with progressive refinement later.

## Background Work

Scanning stays off the UI thread. The UI thread handles Win32 messages, rendering, and small command dispatch only. Scan workers send progress snapshots and final graph handles through bounded channels. Direct NTFS drive scans request UAC relaunch before scan work starts.

## Verification

Each native milestone should include:

- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- release build of `diskloom`, `diskloom-cli`, `diskloom-ui`, and `diskloom-bench`
- manual scan of a drive root and a folder
- Task Manager working-set check while idle and after a large scan
- `diskloom-bench inspect C:\ --scanner ntfs --output ...` to verify root shape

DiskLoom should not claim to beat WizTree until same-machine benchmark artifacts show it.
