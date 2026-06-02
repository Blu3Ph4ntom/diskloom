# Slint Analyzer UI Design

DiskLoom's next GUI slice should use Slint for a modest, polished analyzer-first interface. The first Slint release should focus on one workflow: scan a path or drive, then inspect an expandable folder/file tree sorted by size.

## Scope

The UI shows:

- title and scan status
- scan path field
- discovered drive shortcuts
- scanner mode selector
- scan button and cancel button
- expandable folder/file rows with size, allocated size, kind, and child count

The UI does not include treemap, duplicate search, export, rename, or delete in this slice. Those actions stay in backend and CLI crates until the Slint tree is stable.

## Behavior

The scanner logic remains independent from the UI. The Slint layer starts background scans and receives progress/final results through Slint's event-loop handoff. Direct NTFS drive scans keep the existing UAC relaunch behavior.

The tree model stores compact row data:

- `EntryId`
- display name
- depth
- size and allocated display strings
- kind
- folder/expanded flags
- child count

The UI never stores full path strings per row. Paths remain lazy backend data.

## Visual Direction

The interface should be modest and quiet:

- dark neutral shell
- restrained green accent for the scan action and selected rows
- dense rows with clear alignment
- no decorative cards, gradients, hero sections, or marketing copy
- system-style typography and predictable spacing

## Verification

The slice is complete when:

- `diskloom-ui` builds with Slint
- scans run from the GUI
- folders expand/collapse
- file and folder rows show full visible hierarchy
- workspace tests and clippy pass
- release executables and portable package are rebuilt
