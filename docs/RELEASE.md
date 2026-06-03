# Release Notes

## DiskLoom 0.1.1

First public Windows preview.

### Included

- `diskloom.exe`: Windows desktop disk analyzer.
- `dlm.exe`: command-line analyzer.
- `DiskLoomSetup-x64.exe`: native Windows installer that installs both binaries and adds DiskLoom to PATH.

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
