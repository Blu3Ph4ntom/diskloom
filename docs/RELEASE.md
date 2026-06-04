# Release Notes

## DiskLoom 0.1.2

Disk usage reconciliation update.

### Included

- `diskloom.exe`: Windows desktop disk analyzer.
- `dlm.exe`: command-line analyzer.
- `DiskLoomSetup-x64.exe`: native Windows installer that installs both binaries and adds DiskLoom to PATH.

### Changed

- Whole-drive scans now reconcile scanned file-tree allocation against Windows reported used space.
- The app shows an `Unattributed / system reserved` row when used space cannot be mapped to normal file entries.
- Scan details include Windows used space and unattributed space.
- Fixed Tauri installer build configuration for local release packaging.

### Install

Download `DiskLoomSetup-x64.exe` from the latest release and run the installer.

For terminal use after installation:

```powershell
dlm
dlm C:\ --scanner auto --limit 25
dlm C:\Users --csv users.csv
```

### Notes

- Direct NTFS scans can require administrator access.
- Fallback traversal works without elevation.
- DiskLoom has no telemetry and no background service.
