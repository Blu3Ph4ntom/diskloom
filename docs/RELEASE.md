# Release Notes

## DiskLoom 0.1.3

NTFS deleted-file visibility update.

### Included

- `diskloom.exe`: Windows desktop disk analyzer.
- `dlm.exe`: command-line analyzer.
- `DiskLoomSetup-x64.exe`: native Windows installer that installs both binaries and adds DiskLoom to PATH.

### Changed

- Direct NTFS scans now include the `$Extend` metadata tree, including `$Extend\$Deleted`, so deleted-but-open files can be identified instead of appearing as unexplained used space.
- CLI whole-drive scans now show scanned logical size, scanned allocated size, Windows reported used/total space, and any remaining unaccounted allocation.
- Improved scan output for diagnosing disk-full cases where Explorer cannot show the file consuming space.

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
